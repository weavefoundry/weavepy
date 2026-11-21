"""`functools.cmp_to_key`, and calls of callables held in `__slots__`.

`cmp_to_key(f)` returns a key factory that makes no class per call; its
keys compare through `f`, keep `.obj`, take `obj` by keyword, report the
signature `(obj)`, are unhashable, and fail against other objects.
Compiled code calls `self.fn(...)` of a function a `__slots__` member
holds without leaving; an unset member, a member assigned a new callable,
a subclass that shadows the name with a method, and a class whose layout
changes behave as before.
"""

import collections.abc
import functools
import inspect
import unittest

N = 3000


class Holder:
    __slots__ = ("fn", "other")

    def __init__(self, fn):
        self.fn = fn

    def run(self, x):
        return self.fn(x)


class Shadow(Holder):
    def __init__(self):
        pass

    def fn(self, x):
        return ("method", x)


class CmpToKeyTests(unittest.TestCase):
    def test_sorting(self):
        for _ in range(N // 10):
            r = sorted(range(10), key=functools.cmp_to_key(lambda a, b: b - a))
            s = sorted([5, "3", 7, 2, "0"], key=functools.cmp_to_key(lambda x, y: int(x) - int(y)))
        self.assertEqual(r, list(range(9, -1, -1)))
        self.assertEqual(s, ["0", 2, "3", 5, 7])

    def test_key_objects(self):
        def cmp1(x, y):
            return (x > y) - (x < y)

        key = functools.cmp_to_key(mycmp=cmp1)
        self.assertEqual(key(obj=3), key(obj=3))
        self.assertGreater(key(3), key(1))
        self.assertLessEqual(key(1), key(1))
        self.assertEqual(key(50).obj, 50)
        self.assertEqual(str(inspect.signature(key)), "(obj)")
        with self.assertRaises((TypeError, AttributeError)):
            key(3) > 1
        with self.assertRaises((TypeError, AttributeError)):
            1 < key(3)
        with self.assertRaises(TypeError):
            hash(key(10))
        self.assertNotIsInstance(key(1), collections.abc.Hashable)
        with self.assertRaises(TypeError):
            functools.cmp_to_key()
        with self.assertRaises(TypeError):
            key()

    def test_raising_cmp(self):
        def bad(x, y):
            raise ZeroDivisionError

        key = functools.cmp_to_key(bad)
        for _ in range(200):
            with self.assertRaises(ZeroDivisionError):
                key(3) > key(1)
            with self.assertRaises(ZeroDivisionError):
                sorted([3, 1, 2], key=key)


class SlotCallableTests(unittest.TestCase):
    def test_slot_function(self):
        h = Holder(lambda x: x * 2)
        for i in range(N):
            r = h.run(i)
        self.assertEqual(r, (N - 1) * 2)

    def test_reassigned_and_unset(self):
        h = Holder(len)
        out = []
        for i in range(N):
            if i == N // 2:
                h.fn = str
            out.append(h.run([1, 2]))
        self.assertEqual((out[0], out[-1]), (2, "[1, 2]"))
        e = Holder.__new__(Holder)
        for _ in range(200):
            with self.assertRaises(AttributeError):
                e.run(1)

    def test_shadowing_subclass(self):
        items = [Holder(lambda x: ("slot", x)), Shadow()]
        out = []
        for i in range(N):
            out.append(items[i % 2].run(i))
        self.assertEqual(out[:2], [("slot", 0), ("method", 1)])

    def test_mixed_receivers(self):
        class Plain:
            def __init__(self, fn):
                self.fn = fn

            def run(self, x):
                return self.fn(x)

        items = [Holder(abs), Plain(abs), Holder(lambda x: -x)]
        out = []
        for i in range(N):
            out.append(items[i % 3].run(-i))
        self.assertEqual(out[:3], [0, 1, 2])
        self.assertEqual(out[-3:], [N - 3, N - 2, N - 1])


if __name__ == "__main__":
    unittest.main()

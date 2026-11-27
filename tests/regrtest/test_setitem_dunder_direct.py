"""Compiled item stores call a class's Python `__setitem__` directly; the
items stored, the method's ignored result, raises (caught in the storing
frame or past it), stores from inside methods and generators, the
operands' lifetimes, and a method replaced later match the general
store."""

import collections
import sys
import unittest
import weakref


class Logged:
    def __init__(self):
        self.data = {}
        self.calls = 0

    def __setitem__(self, k, v):
        # More than a dict delegate: a count and a store.
        self.calls = self.calls + 1
        self.data[k] = v
        return "ignored"


class Checked(dict):
    def __setitem__(self, k, v):
        if not isinstance(k, str):
            raise TypeError("keys are str")
        super().__setitem__(k, v * 2)


class Tracked:
    deleted = 0

    def __del__(self):
        Tracked.deleted += 1


class Holder:
    def __init__(self):
        self.d = {}

    def __setitem__(self, k, v):
        self.d[k] = v
        self.last = k

    def fill(self, n):
        for i in range(n):
            self[i] = i * i


class SetitemDunderDirect(unittest.TestCase):
    def test_logged(self):
        o = Logged()
        for i in range(3000):
            o[i % 7] = i
        self.assertEqual(o.calls, 3000)
        self.assertEqual(o.data[2999 % 7], 2999)
        self.assertEqual(len(o.data), 7)

    def test_dict_subclass(self):
        c = Checked()
        for i in range(3000):
            c["k%d" % (i % 5)] = i
        self.assertEqual(c["k4"], 2 * 2999)
        self.assertEqual(len(c), 5)

    def test_raise_caught_here(self):
        c = Checked()
        n = 0
        for i in range(2000):
            try:
                c[i] = i
            except TypeError as e:
                n += 1
                self.assertEqual(str(e), "keys are str")
        self.assertEqual(n, 2000)
        self.assertEqual(len(c), 0)

    def test_raise_past_frame(self):
        def store(c, k, v):
            c[k] = v
            return "after"

        c = Checked()
        for i in range(2000):
            with self.assertRaises(TypeError):
                store(c, i, i)
            self.assertEqual(store(c, "a", i), "after")
        self.assertEqual(c["a"], 2 * 1999)

    def test_self_stores(self):
        h = Holder()
        for _ in range(300):
            h.fill(20)
        self.assertEqual(h.d[19], 361)
        self.assertEqual(h.last, 19)

    def test_generator_caller(self):
        o = Logged()

        def g():
            for i in range(500):
                o[i] = -i
                yield i

        self.assertEqual(sum(g()), sum(range(500)))
        self.assertEqual(o.data[499], -499)

    def test_ordered_dict_and_chainmap(self):
        for _ in range(300):
            od = collections.OrderedDict()
            od["a"] = 1
            od["b"] = 2
            od["a"] = 3
            self.assertEqual(list(od.items()), [("a", 3), ("b", 2)])
            cm = collections.ChainMap({}, {"x": 1})
            cm["y"] = 2
            cm["x"] = 5
            self.assertEqual(cm.maps[0], {"y": 2, "x": 5})
            ud = collections.UserDict()
            ud["q"] = 1
            self.assertEqual(ud.data, {"q": 1})

    def test_lifetimes(self):
        h = Holder()
        for i in range(500):
            h["t"] = Tracked()
        before = Tracked.deleted
        t = Tracked()
        r = weakref.ref(t)
        h["t"] = t
        del t
        self.assertIsNotNone(r())
        h["t"] = None
        self.assertIsNone(r())
        self.assertEqual(Tracked.deleted, before + 2)
        n = sys.getrefcount(h)
        for i in range(1000):
            h["key"] = i
        self.assertEqual(sys.getrefcount(h), n)

    def test_method_replaced(self):
        class C:
            def __init__(self):
                self.d = {}

            def __setitem__(self, k, v):
                self.d[k] = v + 1

        c = C()
        for i in range(1000):
            c[0] = i
        self.assertEqual(c.d[0], 1000)

        def other(self, k, v):
            self.d[k] = -v

        C.__setitem__ = other
        for i in range(1000):
            c[0] = i
        self.assertEqual(c.d[0], -999)
        del C.__setitem__
        with self.assertRaises(TypeError):
            c[0] = 1


if __name__ == "__main__":
    unittest.main()

"""WeakKeyDictionary and WeakSet lookups served natively for keys that
compare by identity, from compiled loops: classes, functions and plain
instances as keys, get with and without a default, membership, updates
of existing keys and inserts of new ones, keys that die, keys whose
class defines __eq__ and __hash__ (the Python path's), a subclass that
overrides __getitem__, functools.singledispatch through its weak cache,
and a profiler that sees the Python methods, each matching CPython."""

import functools
import gc
import sys
import unittest
import weakref


class O:
    pass


class Eq:
    def __init__(self, v):
        self.v = v

    def __eq__(self, o):
        return isinstance(o, Eq) and self.v == o.v

    def __hash__(self):
        return hash(self.v)


def lookups(n, d, k):
    out = None
    for i in range(n):
        out = (d[k], d.get(k), d.get(k, "dflt"), k in d)
    return out


class WeakKeyDictionaryTests(unittest.TestCase):
    def test_identity_keys(self):
        a, b = O(), O()

        def f():
            pass

        d = weakref.WeakKeyDictionary({a: 1, O: "cls", f: "fn"})
        for n in (1, 300):
            self.assertEqual(lookups(n, d, a), (1, 1, 1, True))
            self.assertEqual(lookups(n, d, O), ("cls", "cls", "cls", True))
            self.assertEqual(lookups(n, d, f), ("fn", "fn", "fn", True))
            self.assertEqual((d.get(b), d.get(b, 0), b in d), (None, 0, False))
            with self.assertRaises(KeyError):
                for i in range(n):
                    d[b]

    def test_stores(self):
        a, b = O(), O()
        d = weakref.WeakKeyDictionary()
        for n in (1, 300):
            for i in range(n):
                d[a] = i
            self.assertEqual(d[a], n - 1)
            self.assertEqual(len(d), 1)
        for i in range(300):
            d[b] = i
        self.assertEqual(len(d), 2)
        del b
        gc.collect()
        self.assertEqual(len(d), 1)
        self.assertEqual(list(d.items()), [(a, 299)])

    def test_equal_keys(self):
        d = weakref.WeakKeyDictionary()
        k1, k2 = Eq(1), Eq(1)
        d[k1] = "one"
        for n in (1, 300):
            self.assertEqual(lookups(n, d, k2), ("one", "one", "one", True))
        for i in range(300):
            d[k2] = "two"
        self.assertEqual(len(d), 1)
        self.assertEqual(d[k1], "two")

    def test_subclass_override(self):
        class Sub(weakref.WeakKeyDictionary):
            def __getitem__(self, key):
                return "sub"

        a = O()
        d = Sub({a: 1})
        for i in range(300):
            self.assertEqual(d[a], "sub")
            self.assertEqual(d.get(a), 1)


class WeakSetTests(unittest.TestCase):
    def test_add_and_contains(self):
        a, b = O(), O()
        s = weakref.WeakSet()
        for n in (1, 300):
            for i in range(n):
                s.add(a)
            self.assertEqual(len(s), 1)
            self.assertTrue(all(a in s for i in range(n)))
            self.assertFalse(b in s)
        s.add(b)
        self.assertEqual(len(s), 2)
        del b
        gc.collect()
        self.assertEqual(len(s), 1)
        k1, k2 = Eq(3), Eq(3)
        s.add(k1)
        self.assertIn(k2, s)
        self.assertFalse(1 in s)


class WeakValueDictionaryTests(unittest.TestCase):
    def test_stores_and_removal(self):
        vals = [O() for _ in range(3)]
        d = weakref.WeakValueDictionary()
        for n in (1, 300):
            for i in range(n):
                d["a"] = vals[0]
                d[i % 2] = vals[1]
            self.assertIs(d["a"], vals[0])
            self.assertIs(d[0], vals[1])
        refs = list(d.valuerefs())
        self.assertTrue(all(isinstance(r, weakref.KeyedRef) for r in refs))
        self.assertEqual(sorted(map(repr, (r.key for r in refs))), sorted(map(repr, d.keys())))
        d["c"] = vals[2]
        self.assertEqual(len(d), 4)
        del vals[2]
        gc.collect()
        self.assertNotIn("c", d)
        self.assertEqual(len(d), 3)
        with self.assertRaises(TypeError):
            for i in range(300):
                d["x"] = 1


class SingleDispatchTests(unittest.TestCase):
    def test_dispatch(self):
        @functools.singledispatch
        def show(x):
            return "obj"

        @show.register
        def _(x: int):
            return "int"

        @show.register
        def _(x: str):
            return "str"

        def calls(n, x):
            out = None
            for i in range(n):
                out = show(x)
            return out

        for n in (1, 300):
            self.assertEqual(calls(n, 1), "int")
            self.assertEqual(calls(n, "a"), "str")
            self.assertEqual(calls(n, 1.5), "obj")
            self.assertEqual(calls(n, True), "int")

        @show.register
        def _(x: float):
            return "float"

        self.assertEqual(calls(300, 1.5), "float")


class ProfilerTests(unittest.TestCase):
    def test_profiler_sees_python_methods(self):
        a = O()
        d = weakref.WeakKeyDictionary({a: 1})
        lookups(300, d, a)
        seen = []

        def prof(frame, event, arg):
            if event == "call":
                seen.append(frame.f_code.co_name)

        sys.setprofile(prof)
        try:
            d[a]
        finally:
            sys.setprofile(None)
        self.assertIn("__getitem__", seen)


if __name__ == "__main__":
    unittest.main()

"""Compiled `in` / `not in` tests call a class's Python `__contains__`
directly, and compiled `del obj[k]` its Python `__delitem__`; the
answers (a non-`bool` answer's truth included, a `__bool__` there too),
raises (caught in the frame or past it), the operands' lifetimes, and
methods replaced later match the general operators."""

import unittest
import weakref


class Bag:
    def __init__(self, *items):
        self.items = dict.fromkeys(items)
        self.calls = 0

    def __contains__(self, x):
        self.calls += 1
        return x in self.items

    def __delitem__(self, k):
        self.calls += 1
        del self.items[k]


class Answer:
    def __init__(self, v):
        self.v = v

    def __bool__(self):
        return self.v


class Odd:
    def __contains__(self, x):
        if x == "raise":
            raise KeyError(x)
        if x == "answer":
            return Answer(False)
        return x if isinstance(x, int) else []


class Tracked:
    deleted = 0

    def __del__(self):
        Tracked.deleted += 1


class ContainsDelitemDirect(unittest.TestCase):
    def test_contains(self):
        b = Bag("a", "b")
        n = 0
        for i in range(3000):
            if "a" in b:
                n += 1
            if "z" in b:
                n += 100
            if "z" not in b:
                n += 1
        self.assertEqual(n, 6000)
        self.assertEqual(b.calls, 9000)

    def test_contains_values(self):
        b = Bag("a")
        for i in range(2000):
            self.assertIs("a" in b, True)
            self.assertIs("q" not in b, True)
            key = "a" if i % 2 else "b"
            self.assertEqual(key in b, i % 2 == 1)

    def test_non_bool_answers(self):
        o = Odd()
        for i in range(1000):
            self.assertIs(3 in o, True)
            self.assertIs(0 in o, False)
            self.assertIs("s" in o, False)
            self.assertIs("s" not in o, True)
            self.assertIs("answer" in o, False)
            self.assertIs("answer" not in o, True)

    def test_contains_raises(self):
        o = Odd()
        n = 0
        for i in range(1000):
            try:
                if "raise" in o:
                    n += 100
            except KeyError:
                n += 1
        self.assertEqual(n, 1000)

        def check(c, x):
            return x in c

        for i in range(1000):
            with self.assertRaises(KeyError):
                check(o, "raise")
            self.assertTrue(check(o, 1))

    def test_delitem(self):
        for _ in range(300):
            b = Bag(*range(10))
            for k in range(0, 10, 2):
                del b[k]
            self.assertEqual(list(b.items), [1, 3, 5, 7, 9])
            self.assertEqual(b.calls, 5)

    def test_delitem_raises(self):
        b = Bag()
        n = 0
        for i in range(1000):
            try:
                del b[i]
            except KeyError:
                n += 1
        self.assertEqual(n, 1000)

        def drop(c, k):
            del c[k]
            return "after"

        for i in range(1000):
            b.items[i] = i
            self.assertEqual(drop(b, i), "after")
            with self.assertRaises(KeyError):
                drop(b, i)

    def test_lifetimes(self):
        b = Bag()
        for i in range(500):
            b.items["t"] = Tracked()
            del b["t"]
        self.assertGreaterEqual(Tracked.deleted, 500)
        t = Tracked()
        r = weakref.ref(t)
        b.items["t"] = t
        del t
        self.assertIsNotNone(r())
        del b["t"]
        self.assertIsNone(r())
        probe = Tracked()
        pr = weakref.ref(probe)
        for i in range(500):
            probe in b
        del probe
        self.assertIsNone(pr())

    def test_replaced(self):
        class C:
            def __contains__(self, x):
                return True

            def __delitem__(self, k):
                self.last = k

        c = C()
        n = 0
        for i in range(1000):
            if i in c:
                n += 1
            del c[i]
        self.assertEqual((n, c.last), (1000, 999))
        C.__contains__ = lambda self, x: False
        C.__delitem__ = lambda self, k: setattr(self, "last", -k)
        for i in range(1000):
            if i in c:
                n += 100
            del c[i]
        self.assertEqual((n, c.last), (1000, -999))
        del C.__contains__
        with self.assertRaises(TypeError):
            1 in c


if __name__ == "__main__":
    unittest.main()

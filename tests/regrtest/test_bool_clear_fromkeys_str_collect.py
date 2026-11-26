"""bool() of plain values and containers (its constructor arm first),
list()/set()/frozenset()/tuple() of a str's characters collected
directly, container constructors from lists and tuples, list.clear
as a leaf over plain items, and dict.fromkeys over a short range, from
compiled loops: results for empty and non-empty values, non-ASCII and
astral characters, a list of instances cleared (the full path, their
finalizers running), ranges with steps, long ranges, and dict subclasses,
each matching CPython."""

import unittest


class Fin:
    count = 0

    def __del__(self):
        Fin.count += 1


class D(dict):
    pass


class BuiltinBatchTests(unittest.TestCase):
    def test_bool(self):
        vals = [0, 1, 0.0, 2.5, "", "a", b"", None, (), (1,), [], [0], {}, {1: 2}, set(), {1}]
        for i in range(300):
            r = [bool(v) for v in vals]
        self.assertEqual(r, [bool(v) for v in vals])
        self.assertEqual(r[:6], [False, True, False, True, False, True])
        self.assertIs(bool(frozenset()), False)

    def test_str_collect(self):
        for i in range(300):
            a = list("hello")
            b = set("hello")
            c = tuple("hé\U0001f600")
            d = list("")
        self.assertEqual(a, ["h", "e", "l", "l", "o"])
        self.assertEqual(b, {"h", "e", "l", "o"})
        self.assertEqual(c, ("h", "é", "\U0001f600"))
        self.assertEqual(d, [])
        self.assertIs(a[2], a[3])
        for i in range(300):
            e = frozenset("abca")
            f = set("x" * 100)
            g = dict([("a", 1), ("b", 2)])
            h = list((1, 2))
            t = tuple([3, 4])
        self.assertEqual((e, f), (frozenset("abc"), {"x"}))
        self.assertIs(type(e), frozenset)
        self.assertEqual((g, h, t), ({"a": 1, "b": 2}, [1, 2], (3, 4)))

    def test_clear(self):
        for i in range(300):
            xs = [1, "a", 2.5, None, b"x"]
            xs.clear()
        self.assertEqual(xs, [])
        Fin.count = 0
        for i in range(300):
            ys = [Fin(), Fin()]
            ys.clear()
        self.assertEqual(Fin.count, 600)
        it = iter([1, 2, 3])
        zs = [1, 2, 3]
        it = iter(zs)
        next(it)
        zs.clear()
        self.assertEqual(list(it), [])

    def test_fromkeys_range(self):
        for i in range(300):
            a = dict.fromkeys(range(5))
            b = dict.fromkeys(range(10, 0, -3), 0)
            c = dict.fromkeys(range(100))
            d = D.fromkeys(range(3), "x")
        self.assertEqual(a, {0: None, 1: None, 2: None, 3: None, 4: None})
        self.assertEqual(list(b), [10, 7, 4, 1])
        self.assertEqual(len(c), 100)
        self.assertIs(type(d), D)
        self.assertEqual(d, {0: "x", 1: "x", 2: "x"})
        self.assertEqual(dict.fromkeys(range(0)), {})
        self.assertEqual(list(dict.fromkeys(range(2**62, 2**62 + 2))), [2**62, 2**62 + 1])


if __name__ == "__main__":
    unittest.main()

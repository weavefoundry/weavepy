"""itertools.compress stepped in place, bisect with a key called in
place, and OrderedDict built from pairs or a dict, from compiled loops:
selectors of every plain kind and of other kinds, the same iterator as
both arguments, unequal lengths, keys that are itemgetters, lambdas and
raising functions, insort with a key, and OrderedDict order, duplicate
keys, mixed and non-plain keys, a subclass and the profile of a class
whose __init__ changed, each matching CPython."""

import bisect
import collections
import itertools
import operator
import unittest


def compress(n, data, sel):
    out = None
    for i in range(n):
        out = list(itertools.compress(data, sel))
    return out


class CompressTests(unittest.TestCase):
    def test_selectors(self):
        for n in (1, 300):
            self.assertEqual(compress(n, [1, 2, 3, 4], [1, 0, 1, 0]), [1, 3])
            self.assertEqual(compress(n, "abcd", [True, None, 2, False]), ["a", "c"])
            self.assertEqual(compress(n, (1, 2, 3), [1, 1]), [1, 2])
            self.assertEqual(compress(n, [1, 2], [1, 1, 1]), [1, 2])
            self.assertEqual(compress(n, range(5), [0, "", "x", [], [0]]), [2, 4])
            self.assertEqual(compress(n, [], []), [])

    def test_same_iterator(self):
        for _ in range(300):
            it = iter([1, 0, 2, 1, 3, 0])
            self.assertEqual(list(itertools.compress(it, it)), [2])

    def test_partial_then_resume(self):
        for _ in range(300):
            c = itertools.compress([1, 2, 3, 4], [1, 1, 0, 1])
            self.assertEqual(next(c), 1)
            self.assertEqual(list(c), [2, 4])


def bisects(n, xs, x, key):
    out = None
    for i in range(n):
        out = (bisect.bisect_left(xs, x, key=key), bisect.bisect_right(xs, x, key=key))
    return out


class BisectKeyTests(unittest.TestCase):
    def test_keys(self):
        xs = [(1, "a"), (3, "b"), (3, "c"), (5, "d")]
        for n in (1, 300):
            self.assertEqual(bisects(n, xs, 3, operator.itemgetter(0)), (1, 3))
            self.assertEqual(bisects(n, xs, 4, lambda t: t[0]), (3, 3))
            self.assertEqual(bisects(n, ["a", "bb", "ccc"], 2, len), (1, 2))

        def bad(t):
            raise RuntimeError("key")

        with self.assertRaises(RuntimeError):
            bisects(300, xs, 3, bad)

    def test_insort_key(self):
        for _ in range(300):
            xs = [(1, "a"), (5, "d")]
            bisect.insort(xs, (3, "x"), key=operator.itemgetter(0))
            self.assertEqual(xs, [(1, "a"), (3, "x"), (5, "d")])


class Sub(collections.OrderedDict):
    pass


def ods(n, arg, cls=collections.OrderedDict):
    out = None
    for i in range(n):
        out = cls(arg)
    return out


class OrderedDictCtorTests(unittest.TestCase):
    def test_pairs_and_dicts(self):
        for n in (1, 300):
            o = ods(n, [("b", 1), ("a", 2), (3, "c")])
            self.assertEqual(list(o.items()), [("b", 1), ("a", 2), (3, "c")])
            o.move_to_end("b")
            self.assertEqual(list(o), ["a", 3, "b"])
            self.assertEqual(list(ods(n, (("x", 1), ("x", 2)))), ["x"])
            self.assertEqual(ods(n, (("x", 1), ("x", 2)))["x"], 2)
            self.assertEqual(list(ods(n, {"q": 1, 2: 3}).items()), [("q", 1), (2, 3)])
            self.assertEqual(list(ods(n, [(1.5, "f"), ((1, 2), "t")])), [1.5, (1, 2)])
            self.assertEqual(list(ods(n, [["k", "v"]])), ["k"])
            s = ods(n, [("z", 0)], Sub)
            self.assertIs(type(s), Sub)
            self.assertEqual(list(s), ["z"])
        with self.assertRaises(ValueError):
            ods(300, [("a", 1, 2)])
        with self.assertRaises(TypeError):
            ods(300, [1])


if __name__ == "__main__":
    unittest.main()

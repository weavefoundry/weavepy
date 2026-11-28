"""list(map(f, xs)) where f is an instance with a native __call__
(itemgetter, attrgetter, methodcaller, partial) runs its fast half per
item, and zip_longest builds narrow rows in place: results, a getter
that raises partway (KeyError, IndexError, AttributeError), multi-key
getters, a partial over a Python function, and zip_longest over one to
five sources of unequal lengths with fill values, each matching
CPython."""

import functools
import itertools
import operator
import unittest


class P:
    def __init__(self, x):
        self.x = x


class NativeCallMapTests(unittest.TestCase):
    def test_getters(self):
        pairs = [(1, "a"), (2, "b"), (3, "c")]
        for i in range(300):
            r = list(map(operator.itemgetter(0), pairs))
            s = list(map(operator.itemgetter(1, 0), pairs))
            t = list(map(operator.attrgetter("x"), [P(1), P(2)]))
            u = list(map(operator.methodcaller("upper"), ["a", "b"]))
            v = list(map(functools.partial(operator.add, 10), [1, 2]))
            w = list(map(functools.partial(lambda a, b: a * b, 3), [1, 2]))
        self.assertEqual(r, [1, 2, 3])
        self.assertEqual(s, [("a", 1), ("b", 2), ("c", 3)])
        self.assertEqual((t, u, v, w), ([1, 2], ["A", "B"], [11, 12], [3, 6]))

    def test_errors(self):
        for i in range(300):
            with self.assertRaises(KeyError):
                list(map(operator.itemgetter("k"), [{"k": 1}, {}]))
            with self.assertRaises(IndexError):
                list(map(operator.itemgetter(2), [(1, 2, 3), (1,)]))
            with self.assertRaises(AttributeError):
                list(map(operator.attrgetter("x"), [P(1), object()]))


class ZipLongestRowTests(unittest.TestCase):
    def test_widths(self):
        for i in range(300):
            a = list(itertools.zip_longest([1, 2, 3], "ab"))
            b = list(itertools.zip_longest([1], [], (2, 3), fillvalue=0))
            c = list(itertools.zip_longest([1, 2]))
            d = list(itertools.zip_longest([1], [2], [3], [4, 5]))
            e = list(itertools.zip_longest([1], [2], [3], [4], [5, 6], fillvalue="-"))
        self.assertEqual(a, [(1, "a"), (2, "b"), (3, None)])
        self.assertEqual(b, [(1, 0, 2), (0, 0, 3)])
        self.assertEqual(c, [(1,), (2,)])
        self.assertEqual(d, [(1, 2, 3, 4), (None, None, None, 5)])
        self.assertEqual(e, [(1, 2, 3, 4, 5), ("-", "-", "-", "-", 6)])
        it = iter([1, 2])
        self.assertEqual(list(itertools.zip_longest(it, it)), [(1, 2)])


if __name__ == "__main__":
    unittest.main()

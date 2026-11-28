"""Counter(list) and Counter(tuple) count a run of plain keys under one
view of the dict, and list(zip(...)) of list and tuple cursors reads them
in place: counts and first-seen order, keys whose __eq__ runs Python
partway through, non-int counts already present, Counter.update over a
dict subclass, zips of unequal lengths with shared and partly consumed
cursors, each matching CPython."""

import collections
import unittest


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and self.v == other.v


class CounterRunTests(unittest.TestCase):
    def test_counts_and_order(self):
        words = "b a b c a b".split()
        for i in range(300):
            c = collections.Counter(words)
            t = collections.Counter(tuple(words))
        self.assertEqual(c, {"b": 3, "a": 2, "c": 1})
        self.assertEqual(list(c), ["b", "a", "c"])
        self.assertEqual(t, c)
        self.assertEqual(collections.Counter([1, 2, 1, True, 2.0]), {1: 3, 2: 2})

    def test_mixed_keys(self):
        k = Key(1)
        items = ["x", "y", k, "x", Key(1), 3, 3]
        c = collections.Counter(items)
        self.assertEqual(c["x"], 2)
        self.assertEqual(c[k], 2)
        self.assertEqual(c[3], 2)
        self.assertEqual(list(c), ["x", "y", k, 3])

    def test_existing_counts(self):
        c = collections.Counter({"a": 1.5, "b": 2})
        c.update(["b", "a", "b", "z"])
        self.assertEqual(c, {"a": 2.5, "b": 4, "z": 1})
        self.assertIsInstance(c["a"], float)
        c.update(("z", "z"))
        self.assertEqual(c["z"], 3)

    def test_subclass(self):
        class C(collections.Counter):
            pass

        c = C("abca")
        c.update(["a", "q"])
        self.assertEqual(c, {"a": 3, "b": 1, "c": 1, "q": 1})


class ZipViewTests(unittest.TestCase):
    def test_lengths_and_cursors(self):
        a = [1, 2, 3, 4]
        b = (5, 6)
        for i in range(300):
            r = list(zip(a, b))
            m = list(zip(*[[1, 2], [3, 4], [5, 6]]))
        self.assertEqual(r, [(1, 5), (2, 6)])
        self.assertEqual(m, [(1, 3, 5), (2, 4, 6)])
        x, y = iter(a), iter(b)
        next(x)
        self.assertEqual(list(zip(x, y)), [(2, 5), (3, 6)])
        self.assertEqual(list(x), [])
        x, y = iter(b), iter(a)
        self.assertEqual(list(zip(x, y)), [(5, 1), (6, 2)])
        self.assertEqual(list(y), [3, 4])
        s = iter(a)
        self.assertEqual(list(zip(s, s)), [(1, 2), (3, 4)])
        self.assertEqual(list(zip([], a)), [])


if __name__ == "__main__":
    unittest.main()

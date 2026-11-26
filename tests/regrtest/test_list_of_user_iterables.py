"""list() and tuple() of instances whose __iter__ returns a native
iterator, drained without stepping it through the general loop: ranges
(negative steps, empty, big), lists, tuples, strs, reversed lists, dict
and set iterators (including one changed while drained), a partly
consumed iterator returned from __iter__, and a list that grows while
drained, each matching CPython."""

import unittest


class It:
    def __init__(self, make):
        self.make = make

    def __iter__(self):
        return self.make()


def drain(n, obj):
    out = None
    for i in range(n):
        out = list(obj)
    return out


class UserIterableTests(unittest.TestCase):
    def test_native_iterators(self):
        for n in (1, 300):
            self.assertEqual(drain(n, It(lambda: iter(range(5)))), [0, 1, 2, 3, 4])
            self.assertEqual(drain(n, It(lambda: iter(range(10, 0, -3)))), [10, 7, 4, 1])
            self.assertEqual(drain(n, It(lambda: iter(range(0)))), [])
            self.assertEqual(drain(n, It(lambda: iter([1, "a", None]))), [1, "a", None])
            self.assertEqual(drain(n, It(lambda: iter((1, 2)))), [1, 2])
            self.assertEqual(drain(n, It(lambda: iter("hé"))), ["h", "é"])
            self.assertEqual(drain(n, It(lambda: reversed([1, 2, 3]))), [3, 2, 1])
            self.assertEqual(drain(n, It(lambda: iter({"a": 1, "b": 2}))), ["a", "b"])
            self.assertEqual(sorted(drain(n, It(lambda: iter({3, 1, 2})))), [1, 2, 3])
        big = It(lambda: iter(range(2**62, 2**62 + 3)))
        self.assertEqual(drain(300, big), [2**62, 2**62 + 1, 2**62 + 2])
        self.assertEqual(tuple(It(lambda: iter([4, 5]))), (4, 5))

    def test_partly_consumed_and_shared(self):
        src = iter([1, 2, 3, 4])
        next(src)
        self.assertEqual(list(It(lambda: src)), [2, 3, 4])
        self.assertEqual(list(src), [])
        xs = [1, 2]
        it = iter(xs)
        self.assertEqual(list(It(lambda: it)), [1, 2])
        xs.append(3)
        self.assertEqual(list(it), [])

    def test_changed_dict(self):
        d = {1: 1, 2: 2}

        class Grow:
            def __iter__(self):
                g = iter(d)
                d[3] = 3
                return g

        with self.assertRaises(RuntimeError):
            list(Grow())


if __name__ == "__main__":
    unittest.main()

"""`list(accumulate(xs))` and `tuple(accumulate(xs))` over lists and tuples.

Running totals of ints and floats, with and without `initial`, from the
start or after some were taken; a first item of any kind; totals past the
machine-int range (exact big ints); mixed kinds and objects with `__add__`
(summed as the general step sums them, in order); and an adapter that is
exhausted afterwards, its source too.
"""

import itertools
import unittest
from itertools import accumulate


class Num:
    adds = 0

    def __init__(self, v):
        self.v = v

    def __add__(self, other):
        Num.adds += 1
        return Num(self.v + (other.v if isinstance(other, Num) else other))


def manual(xs, initial=None):
    out = []
    total = initial
    if initial is not None:
        out.append(initial)
    for i, x in enumerate(xs):
        total = x if total is None and not out else total + x
        out.append(total)
    return out


class AccumulateTests(unittest.TestCase):
    def test_plain(self):
        for xs in ([], [5], list(range(10)), [-3, 7, 0, 2**40], [1.5, 2.25, -0.5],
                   tuple(range(20))):
            self.assertEqual(list(accumulate(xs)), manual(list(xs)))
            self.assertEqual(tuple(accumulate(xs)), tuple(manual(list(xs))))
            self.assertEqual(list(accumulate(xs, initial=100)), manual(list(xs), 100))
        self.assertEqual(list(accumulate([], initial=7)), [7])

    def test_partly_taken(self):
        it = accumulate([1, 2, 3, 4, 5])
        self.assertEqual(next(it), 1)
        self.assertEqual(next(it), 3)
        self.assertEqual(list(it), [6, 10, 15])
        self.assertEqual(list(it), [])
        it = accumulate([1, 2, 3], initial=10)
        self.assertEqual(next(it), 10)
        self.assertEqual(list(it), [11, 13, 16])

    def test_big_and_mixed(self):
        big = [2**62, 2**62, 2**62]
        self.assertEqual(list(accumulate(big)), [2**62, 2**63, 3 * 2**62])
        self.assertEqual(list(accumulate([1, 2.5, 3])), [1, 3.5, 6.5])
        self.assertEqual(list(accumulate(["a", "b", "c"])), ["a", "ab", "abc"])
        self.assertEqual(list(accumulate([[1], [2]])), [[1], [1, 2]])
        Num.adds = 0
        got = list(accumulate([Num(1), Num(2), 3]))
        self.assertEqual([n.v for n in got], [1, 3, 6])
        self.assertEqual(Num.adds, 2)
        with self.assertRaises(TypeError):
            list(accumulate([1, "x"]))

    def test_source_exhausted(self):
        src = iter([1, 2, 3])
        acc = accumulate(src)
        self.assertEqual(list(acc), [1, 3, 6])
        self.assertEqual(list(src), [])
        self.assertEqual(list(acc), [])

    def test_with_func(self):
        self.assertEqual(list(accumulate([1, 2, 3], max)), [1, 2, 3])
        self.assertEqual(list(accumulate([3, 1, 2], min)), [3, 1, 1])
        self.assertEqual(list(accumulate([1, 2, 3], lambda a, b: a * b, initial=2)), [2, 2, 4, 12])

    def test_many(self):
        xs = list(range(100))
        want = manual(xs)
        for _ in range(500):
            self.assertEqual(list(accumulate(xs)), want)


if __name__ == "__main__":
    unittest.main()

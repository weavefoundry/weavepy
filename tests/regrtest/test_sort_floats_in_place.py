"""`list.sort()` and `sorted()` of floats order them as `<` does.

Lists of only floats sort stably, forward and in reverse, with infinities,
signed zeros and NaNs placed as CPython's merge sort places them (a NaN
compares false both ways, so where it lands depends on the comparisons
made). Mixed int and float lists still compare exactly.
"""

import math
import random
import unittest


def reference_sort(items, reverse=False):
    """A plain stable merge sort that compares only with `<`."""
    items = list(items)
    if reverse:
        items.reverse()
    def merge(a, b):
        out = []
        i = j = 0
        while i < len(a) and j < len(b):
            if b[j] < a[i]:
                out.append(b[j]); j += 1
            else:
                out.append(a[i]); i += 1
        return out + a[i:] + b[j:]
    def sort(xs):
        if len(xs) <= 1:
            return xs
        mid = len(xs) // 2
        return merge(sort(xs[:mid]), sort(xs[mid:]))
    result = sort(items)
    if reverse:
        result.reverse()
    return result


class FloatSortTests(unittest.TestCase):
    def test_random(self):
        rnd = random.Random(5)
        for n in (0, 1, 2, 10, 100, 1000, 5000):
            xs = [rnd.random() * 100 - 50 for _ in range(n)]
            expected = reference_sort(xs)
            l = xs[:]
            l.sort()
            self.assertEqual(l, expected)
            self.assertEqual(sorted(xs), expected)
            l = xs[:]
            l.sort(reverse=True)
            self.assertEqual(l, expected[::-1])
            self.assertEqual(sorted(xs, reverse=True), expected[::-1])

    def test_specials(self):
        xs = [3.0, -0.0, 0.0, math.inf, -math.inf, 1e-300, -1e300, 2.0, 0.0, -0.0]
        l = xs[:]
        l.sort()
        self.assertEqual([math.copysign(1, x) if x == 0 else x for x in l],
                         [math.copysign(1, x) if x == 0 else x for x in reference_sort(xs)])
        # Zeros keep their order (equal values are stable).
        zeros = [x for x in l if x == 0]
        self.assertEqual([math.copysign(1, z) for z in zeros], [-1.0, 1.0, 1.0, -1.0])
        l = xs[:]
        l.sort(reverse=True)
        zeros = [x for x in l if x == 0]
        self.assertEqual([math.copysign(1, z) for z in zeros], [-1.0, 1.0, 1.0, -1.0])

    def test_stable_identity(self):
        a, b, c = 1.5, 1.5, 0.5
        # Distinct objects with equal values keep their order.
        a = float("1.5"); b = float("1.5")
        l = [a, c, b]
        l.sort()
        self.assertIs(l[1], a)
        self.assertIs(l[2], b)
        l = [a, c, b]
        l.sort(reverse=True)
        self.assertIs(l[0], a)
        self.assertIs(l[1], b)

    def test_nan(self):
        nan = float("nan")
        xs = [3.0, nan, 1.0, 2.0, nan, 0.5]
        l = xs[:]
        l.sort()
        got = ["nan" if x != x else x for x in l]
        want = ["nan" if x != x else x for x in reference_sort(xs)]
        self.assertEqual(len(got), len(want))
        self.assertEqual(sorted(x for x in got if x != "nan"),
                         sorted(x for x in want if x != "nan"))
        self.assertEqual(got.count("nan"), 2)

    def test_mixed_numbers(self):
        xs = [2**53 + 1, float(2**53), 1, 0.5, True, -3, -2.5]
        l = xs[:]
        l.sort()
        self.assertEqual(l, [-3, -2.5, 0.5, 1, True, float(2**53), 2**53 + 1])
        self.assertIs(type(l[3]), int)
        self.assertIs(type(l[4]), bool)


if __name__ == "__main__":
    unittest.main()

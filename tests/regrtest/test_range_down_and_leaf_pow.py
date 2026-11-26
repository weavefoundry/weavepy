"""Loops over descending ranges step in place in both compiled tiers, and
frameless callbacks raise ints to small powers; results match CPython,
overflow and negative exponents included."""

import sys
import unittest


class RangeDownLeafPow(unittest.TestCase):
    def test_descending_ranges(self):
        for _ in range(200):
            self.assertEqual([i for i in range(10, -1, -1)], list(range(10, -1, -1)))
            out = []
            for i in reversed(range(7)):
                out.append(i)
            self.assertEqual(out, [6, 5, 4, 3, 2, 1, 0])
            s = 0
            for i in range(100, 0, -3):
                s += i
            self.assertEqual(s, sum(range(100, 0, -3)))
            self.assertEqual([i for i in range(0, 5, -1)], [])
            self.assertEqual([i for i in range(5, 5, -1)], [])

    def test_extreme_bounds(self):
        big = sys.maxsize
        for _ in range(50):
            self.assertEqual(list(range(big, big - 3, -1)), [big, big - 1, big - 2])
            self.assertEqual(list(range(-big, -big - 3, -1)), [-big, -big - 1, -big - 2])
            self.assertEqual(list(reversed(range(-3, 3, 2))), [1, -1, -3])

    def test_leaf_pow(self):
        xs = list(range(-5, 50))
        for _ in range(200):
            self.assertEqual(min(xs, key=lambda x: (x - 25) ** 2), 25)
            self.assertEqual(sorted(xs, key=lambda x: x ** 3)[:3], [-5, -4, -3])
            self.assertEqual(list(map(lambda x: x ** 0, [3, 0, -2])), [1, 1, 1])
            self.assertEqual(list(map(lambda x: 2 ** x, [10, 62, 63, 64])),
                             [1024, 2 ** 62, 2 ** 63, 2 ** 64])
            self.assertEqual(list(map(lambda x: x ** -1, [2, 4])), [0.5, 0.25])
            self.assertEqual(list(map(lambda x: x ** 2.0, [3])), [9.0])


if __name__ == "__main__":
    unittest.main()

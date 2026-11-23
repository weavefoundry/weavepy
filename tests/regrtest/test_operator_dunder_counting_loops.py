"""Counting loops over instance comparisons and subscripts.

A loop that sums `int`s around `a < b` or `obj[i]` of instances with
Python methods runs where the method call is cheapest (the frame JIT,
which calls or lowers it directly) rather than in tier 2. The results
must match: reflected and `NotImplemented` comparisons, a method rebound
mid-run, mixed operand types at one site, and methods that raise.
"""

import unittest


class V:
    def __init__(self, x):
        self.x = x

    def __lt__(self, other):
        if not isinstance(other, V):
            return NotImplemented
        return self.x < other.x

    def __eq__(self, other):
        return isinstance(other, V) and self.x == other.x

    __hash__ = object.__hash__


class W:
    def __init__(self, x):
        self.x = x

    def __gt__(self, other):
        return self.x > other.x


class Seq:
    def __init__(self, d):
        self.d = d

    def __getitem__(self, i):
        return self.d[i]


def count_lt(a, b, n):
    c = 0
    for _ in range(n):
        if a < b:
            c += 1
    return c


def count_eq(a, b, n):
    c = 0
    for i in range(n):
        if a == b:
            c += i
    return c


def sum_items(s, n):
    t = 0
    for i in range(n):
        t += s[i % 3]
    return t


class CountingLoopTests(unittest.TestCase):
    def test_lt_and_eq(self):
        for _ in range(20):
            self.assertEqual(count_lt(V(1), V(2), 500), 500)
            self.assertEqual(count_lt(V(3), V(2), 500), 0)
            self.assertEqual(count_eq(V(1), V(1), 100), 4950)
            self.assertEqual(count_eq(V(1), 1, 100), 0)

    def test_reflected(self):
        # `V < W`: V.__lt__ declines, W.__gt__ answers.
        for _ in range(20):
            self.assertEqual(count_lt(V(1), W(2), 300), 300)
        with self.assertRaises(TypeError):
            count_lt(V(1), 5, 10)

    def test_rebound(self):
        class R:
            def __lt__(self, other):
                return True

        a, b = R(), R()
        self.assertEqual(count_lt(a, b, 1000), 1000)
        R.__lt__ = lambda self, other: False
        self.assertEqual(count_lt(a, b, 1000), 0)

    def test_subscript_sums(self):
        s = Seq([1, 2, 3])
        for _ in range(20):
            self.assertEqual(sum_items(s, 300), 600)
        self.assertEqual(sum_items([1, 2, 3], 300), 600)
        with self.assertRaises(IndexError):
            sum_items(Seq([1]), 10)


if __name__ == "__main__":
    unittest.main()

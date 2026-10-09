"""`local[i]` of a list read in place by compiled loops.

A loop's `xs[i]` over a list local reads the item where it is. Negative
and out-of-range indexes, a `bool` index, a list subclass, another
sequence in the same local, and a list resized or rebound inside the
loop must all behave as the ordinary subscript does.
"""

import unittest


class L(list):
    def __getitem__(self, i):
        return ("sub", list.__getitem__(self, i))


def total(xs, n):
    s = 0
    for i in range(n):
        s += xs[i % len(xs)] + xs[-1]
    return s


def pick(xs, idx, n):
    out = None
    for _ in range(n):
        out = xs[idx]
    return out


def grow_and_read(n):
    xs = [0]
    acc = 0
    for i in range(n):
        xs.append(i)
        acc += xs[i] + xs[-1]
    return acc


def rebind(n):
    xs = [1, 2, 3]
    acc = []
    for i in range(n):
        if i == n // 2:
            xs = (10, 20, 30)
        acc.append(xs[1])
    return acc


class ListIndexInPlaceTest(unittest.TestCase):
    def test_values(self):
        xs = list(range(10))
        self.assertEqual(total(xs, 5000), sum(i % 10 + 9 for i in range(5000)))
        objs = [object() for _ in range(3)]
        self.assertIs(pick(objs, 1, 3000), objs[1])
        self.assertIs(pick(objs, -3, 3000), objs[0])
        self.assertEqual(pick(["a", "b"], True, 3000), "b")

    def test_out_of_range(self):
        for idx in (3, -4, 10**20):
            with self.assertRaises(IndexError):
                pick([1, 2, 3], idx, 3000)
        with self.assertRaises(TypeError):
            pick([1, 2, 3], "0", 3000)

    def test_other_sequences(self):
        self.assertEqual(pick(L([5, 6]), 0, 3000), ("sub", 5))
        self.assertEqual(pick((7, 8), 1, 3000), 8)
        self.assertEqual(pick("xyz", -1, 3000), "z")
        self.assertEqual(pick({0: "d"}, 0, 3000), "d")

    def test_mutation_and_rebinding(self):
        n = 4000
        self.assertEqual(grow_and_read(n), sum(max(i - 1, 0) + i for i in range(n)))
        acc = rebind(n)
        self.assertEqual(acc[: n // 2], [2] * (n // 2))
        self.assertEqual(acc[n // 2 :], [20] * (n - n // 2))

    def test_refcounts_survive(self):
        import sys

        item = object()
        xs = [item]
        before = sys.getrefcount(item)
        for _ in range(5000):
            y = xs[0]
        del y
        self.assertEqual(sys.getrefcount(item), before)


if __name__ == "__main__":
    unittest.main()

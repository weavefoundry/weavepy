"""itertools.islice over itertools.cycle from compiled loops: the first
pass and the replay of the saved items, an empty source, a cycle partly
consumed first, a generator as the cycle's source, a cycle beside other
sources in zip, and the cycle's state after islice stops, each matching
CPython."""

import itertools
import unittest


def take(n, make, k):
    out = None
    for i in range(n):
        out = list(itertools.islice(make(), k))
    return out


class IsliceCycleTests(unittest.TestCase):
    def test_shapes(self):
        for n in (1, 300):
            self.assertEqual(take(n, lambda: itertools.cycle("ab"), 5), list("ababa"))
            self.assertEqual(take(n, lambda: itertools.cycle([1, 2, 3]), 7), [1, 2, 3, 1, 2, 3, 1])
            self.assertEqual(take(n, lambda: itertools.cycle(()), 3), [])
            self.assertEqual(take(n, lambda: itertools.cycle(x for x in "xy"), 5), list("xyxyx"))
            self.assertEqual(take(n, lambda: zip(itertools.cycle("ab"), range(3)), 5),
                             [("a", 0), ("b", 1), ("a", 2)])

    def test_resume(self):
        for _ in range(300):
            c = itertools.cycle([1, 2, 3])
            self.assertEqual(next(c), 1)
            self.assertEqual(list(itertools.islice(c, 4)), [2, 3, 1, 2])
            self.assertEqual(next(c), 3)
            self.assertEqual(list(itertools.islice(c, 2, 6, 2)), [3, 2])


if __name__ == "__main__":
    unittest.main()

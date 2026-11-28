"""Item stores into an exact dict from compiled loops.

A store by a key that hashes and compares natively goes straight into
the dict, new keys appended in order, displaced values released (their
finalizers running), and iteration over the dict still sees the change.
"""

import unittest

N = 30000


class Fin:
    deaths = 0

    def __del__(self):
        Fin.deaths += 1


D = {"a": 0}


def bump(n):
    for i in range(n):
        D["a"] += 1
        D[i % 7] = i
    return D


class Tier2DictStoreTests(unittest.TestCase):
    def test_counts_and_order(self):
        D.clear()
        D["a"] = 0
        bump(N)
        self.assertEqual(D["a"], N)
        self.assertEqual(list(D), ["a", 0, 1, 2, 3, 4, 5, 6])
        self.assertEqual(D[6], N - 1 - (N - 1 - 6) % 7)

    def test_displaced_values_finalize(self):
        Fin.deaths = 0
        d = {}
        for i in range(N):
            d["k"] = Fin()
        self.assertEqual(Fin.deaths, N - 1)
        del d
        self.assertEqual(Fin.deaths, N)

    def test_mutation_during_iteration(self):
        d = {"x": 1}
        with self.assertRaises(RuntimeError):
            for k in d:
                for i in range(N):
                    d[i] = i


if __name__ == "__main__":
    unittest.main()

"""`for i, x in enumerate(seq)` ending in compiled code.

A loop over `enumerate` of a temporary list or tuple ends without leaving
compiled code when the iterator's release frees only plain values. The
loop must end as the full path ends it: items with finalizers (released
at the next safe point, in order), a shared `enumerate` (a generator
expression's), a list appended to after the end, and a `break`.
"""

import gc
import unittest


class Fin:
    log = []

    def __init__(self, k):
        self.k = k

    def __del__(self):
        Fin.log.append(self.k)


def count(a, b):
    n = 0
    for i, x in enumerate([a, b]):
        n += i
    return n


def count_tuple(a, b):
    n = 0
    for i, x in enumerate((a, b, a)):
        n += i
    return n


def with_fin(k):
    n = 0
    for i, x in enumerate([Fin(k), Fin(k + 1)]):
        n += 1
    return n


class EnumerateEndTests(unittest.TestCase):
    def test_plain_ends(self):
        for _ in range(2000):
            self.assertEqual(count("x", "y"), 1)
            self.assertEqual(count_tuple(1, 2.5), 3)

    def test_finalizers_run(self):
        Fin.log.clear()
        for k in range(0, 400, 2):
            self.assertEqual(with_fin(k), 2)
        gc.collect()
        self.assertEqual(sorted(Fin.log), list(range(400)))

    def test_shared_and_appended(self):
        xs = [1, 2]
        for _ in range(500):
            e = enumerate(xs)
            got = [p for p in e]
            self.assertEqual(got, [(0, 1), (1, 2)])
            self.assertEqual(list(e), [])
        ys = [1]
        it = enumerate(ys)
        for _ in it:
            pass
        ys.append(2)
        self.assertEqual(list(it), [])

    def test_break(self):
        for _ in range(500):
            for i, x in enumerate([7, 8, 9]):
                if x == 8:
                    break
            self.assertEqual(i, 1)


if __name__ == "__main__":
    unittest.main()

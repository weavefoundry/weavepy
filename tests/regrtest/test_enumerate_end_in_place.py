"""`for i, x in enumerate(seq)` ends in place in compiled code.

At the end of a list or tuple, the enumerate iterator leaves the stack
without the general step: its list cursor detaches as CPython's does (an
append after the end can't resume it), and an end whose release could
finalize items keeps the general path, so finalizers still run in order.
"""

import unittest

N = 3000


class Fin:
    deaths = 0

    def __del__(self):
        Fin.deaths += 1


class EnumerateEndTests(unittest.TestCase):
    def test_generator_expressions(self):
        xs = [1, 2, 3]
        t = (4, 5)
        for _ in range(N):
            a = sum(i * v for i, v in enumerate(xs))
            b = [i + v for i, v in enumerate(t)]
        self.assertEqual(a, 8)
        self.assertEqual(b, [4, 6])

    def test_exhausted_iterator_stays_exhausted(self):
        for _ in range(N):
            xs = [1]
            e = enumerate(xs)
            out = [v for _, v in e]
            xs.append(2)
            rest = list(e)
        self.assertEqual((out, rest), ([1], []))

    def test_finalizing_items(self):
        Fin.deaths = 0
        for _ in range(200):
            for _i, _f in enumerate([Fin(), Fin()]):
                pass
            for _i, _f in enumerate((Fin(),)):
                pass
        del _f
        self.assertEqual(Fin.deaths, 600)


if __name__ == "__main__":
    unittest.main()

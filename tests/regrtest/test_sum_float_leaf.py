"""sum() of lists and tuples holding floats, called from compiled loops:
CPython's compensated float loop (entered from the integer loop at the
first float), signed zeros, infinities and NaNs, a machine int too large
to stay one, big ints, and bools, each giving the general call's value."""

import math
import unittest


def sums(n):
    out = None
    for i in range(n):
        out = (
            sum([1.5, 2.5, 3.5]),
            sum((0.1,) * 10),
            sum([1, 2, 0.5]),
            sum([-0.0]),
            sum([-0.0, -0.0]),
            sum([1e308, 1e308]),
            sum([float("inf"), 1.0]),
            sum([2**62, 2**62, 0.5]),
            sum([2**70, 1.5]),
            sum([True, 0.25]),
            sum([1e16, 1.0, -1e16]),
            sum((2**53 + 1, 0.0)),
        )
    return out


def nan_sum(n):
    out = None
    for i in range(n):
        out = sum([float("nan"), 1.0])
    return out


class Tests(unittest.TestCase):
    def test_values(self):
        got = sums(3000)
        want = (
            7.5,
            1.0,
            3.5,
            0.0,
            0.0,
            float("inf"),
            float("inf"),
            float(2**63) + 0.5,
            float(2**70) + 1.5,
            1.25,
            1.0,
            float(2**53 + 1),
        )
        self.assertEqual(got, want)
        # (`0 + -0.0` is positive zero.)
        self.assertEqual(math.copysign(1.0, got[3]), 1.0)
        for g in got:
            self.assertIs(type(g), float)

    def test_nan(self):
        self.assertTrue(math.isnan(nan_sum(2000)))


if __name__ == "__main__":
    unittest.main()

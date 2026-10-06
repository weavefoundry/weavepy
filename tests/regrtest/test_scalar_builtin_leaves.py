"""abs, ord, chr and divmod run in place over plain scalars.

Each takes a fast half for the common shapes, inside loops and inside
small methods evaluated without a frame, and leaves everything else (an
overflowing magnitude, a surrogate, a zero divisor, a wrong type) to the
full builtin, whose results and errors must be unchanged.
"""

import sys
import unittest


class P:
    def __init__(self, x):
        self.x = x

    def dist(self, o):
        return abs(self.x - o.x)

    def code(self, s):
        return ord(s)


class ScalarBuiltinLeafTest(unittest.TestCase):
    def test_abs(self):
        for v, want in [(-3, 3), (3, 3), (0, 0), (-2.5, 2.5), (True, 1), (False, 0)]:
            for _ in range(300):
                self.assertEqual(abs(v), want)
                self.assertIs(type(abs(v)), type(want))
        self.assertEqual(abs(-sys.maxsize - 1), sys.maxsize + 1)
        self.assertEqual(abs(-(2**70)), 2**70)
        self.assertEqual(str(abs(-0.0)), "0.0")
        with self.assertRaises(TypeError):
            abs("x")

    def test_ord_chr(self):
        for _ in range(300):
            self.assertEqual(ord("a"), 97)
            self.assertEqual(ord("é"), 233)
            self.assertEqual(chr(97), "a")
            self.assertEqual(chr(0x1F600), "\U0001F600")
        self.assertEqual(len(chr(0xD800)), 1)
        with self.assertRaises(TypeError):
            ord("ab")
        with self.assertRaises(TypeError):
            ord("")
        with self.assertRaises(ValueError):
            chr(0x110000)
        with self.assertRaises(ValueError):
            chr(-1)

    def test_divmod(self):
        cases = [(7, 2), (-7, 2), (7, -2), (-7, -2), (0, 5), (6, 3), (-6, 3)]
        for a, b in cases:
            for _ in range(100):
                self.assertEqual(divmod(a, b), (a // b, a % b))
        self.assertEqual(divmod(-sys.maxsize - 1, -1), (sys.maxsize + 1, 0))
        self.assertEqual(divmod(7.5, 2), (3.0, 1.5))
        with self.assertRaises(ZeroDivisionError):
            divmod(1, 0)

    def test_in_methods(self):
        p, q = P(1.5), P(4.0)
        total = 0.0
        for _ in range(2000):
            total += p.dist(q)
        self.assertEqual(total, 5000.0)
        self.assertEqual(P(-(2**63)).dist(P(0)), 2**63)
        with self.assertRaises(TypeError):
            p.code("ab")
        self.assertEqual(sum(p.code("b") for _ in range(500)), 98 * 500)


if __name__ == "__main__":
    unittest.main()

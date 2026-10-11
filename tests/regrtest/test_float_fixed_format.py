"""Fixed-point float formatting (`'%.Nf'`, `format(x, '.Nf')`) is
correctly rounded.

Short precisions of moderate magnitudes are formatted in integer
arithmetic; the result must equal the exact binary value rounded half to
even, with the sign kept for negative values that round to zero, at the
edges of that range and past it.
"""

import random
import struct
import unittest
from fractions import Fraction


def reference(x, prec):
    q = Fraction(x) * 10**prec
    n = round(q)  # half to even on the exact value
    neg = x < 0 or (x == 0 and str(x).startswith("-"))
    digits = str(abs(n)).rjust(prec + 1, "0")
    body = digits if prec == 0 else digits[:-prec] + "." + digits[-prec:]
    return ("-" if neg else "") + body


class FixedFormatTest(unittest.TestCase):
    def test_against_exact_rounding(self):
        r = random.Random(5)
        values = [0.0, -0.0, 0.5, 1.5, 2.5, -2.5, 0.125, 1.005, 2.675, 0.45,
                  1e-17, 5e-18, 5e-324, 2.0**53 + 2, 2.0**62, 2.0**63 - 1024,
                  2.0**63, 2.0**64, 1e20, -0.04]
        for _ in range(250):
            values.append(r.uniform(-1e5, 1e5))
            values.append(r.randrange(-10**6, 10**6) / r.choice([8, 1024, 10, 1000]))
            values.append(struct.unpack("<d", struct.pack("<Q", r.getrandbits(64)))[0])
        for v in values:
            if v != v or abs(v) == float("inf") or abs(v) > 1e30:
                continue
            for prec in (0, 1, 3, 6, 17, 18):
                self.assertEqual("%.*f" % (prec, v), reference(v, prec), (v, prec))
                self.assertEqual(format(v, ".%df" % prec), reference(v, prec), (v, prec))

    def test_flags(self):
        self.assertEqual("%8.3f|%-8.2f|%+.1f" % (1.5, -2.25, 0.05), "   1.500|-2.25   |+0.1")
        self.assertEqual(format(-0.0001, "z.2f"), "0.00")
        self.assertEqual(format(-0.0001, ".2f"), "-0.00")
        self.assertEqual(format(2.0, "#.0f"), "2.")
        self.assertEqual(format(1234567.891, ",.2f"), "1,234,567.89")
        self.assertEqual(format(0.5, "010.3f"), "000000.500")


if __name__ == "__main__":
    unittest.main()

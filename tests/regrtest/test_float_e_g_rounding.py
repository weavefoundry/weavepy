"""`e` and `g` formatting of floats rounds the exact binary value.

At every precision from 0 to 20 digits, for floats across the whole
exponent range (zeros, subnormals, values whose shortest form is short and
values whose digits run long), `format(x, '.Ne')` and `'.Ng'` give the
digits `decimal` gives when it rounds the float's exact value half to
even. Inputs are built with `ldexp` from seeded random fractions, and from
decimal literals, so nothing depends on a platform's `pow`.
"""

import decimal
import math
import random
import unittest


def exact_e(x, after):
    """`x` in scientific form with `after` digits past the point."""
    if x == 0:
        # (A decimal zero keeps an exponent of its own: written out here.)
        return ("-" if math.copysign(1, x) < 0 else "") + ("0." + "0" * after if after else "0"), 0
    d = decimal.Decimal(x)
    with decimal.localcontext() as ctx:
        ctx.prec = 1000
        ctx.rounding = decimal.ROUND_HALF_EVEN
        text = format(d, ".%de" % after)
    mant, exp = text.split("e")
    return mant, int(exp)


def split(text):
    mant, exp = text.split("e")
    return mant, int(exp)


VALUES = [0.0, 1.5, 0.1, 0.2, 0.3, 123.456, 2.5, 0.5, 1e22, 1e23, 9.995, 1.0000000000000002,
          0.9999999999999999, 5e-324, 2.2250738585072014e-308, 2.225073858507201e-308,
          1.7976931348623157e308, 1234567890123456.0, 0.000123, 100.0, 1e-7, 3.0, 7.0e15]


class RoundingTests(unittest.TestCase):
    def values(self):
        rnd = random.Random(20261122)
        out = list(VALUES)
        for _ in range(400):
            out.append(math.ldexp(rnd.random(), rnd.randint(-1070, 1020)))
        for _ in range(200):
            # Short decimals: their shortest form has few digits.
            out.append(float("%d.%de%d" % (rnd.randint(1, 999), rnd.randint(0, 99), rnd.randint(-300, 300))))
        return out

    def test_e(self):
        for x in self.values():
            for after in range(0, 21):
                got = split(format(x, ".%de" % after))
                self.assertEqual(got, exact_e(x, after), (x, after))
                self.assertEqual(split(format(-x, ".%de" % after)),
                                 ("-" + got[0], got[1]) if x else (format(-x, ".%de" % after).split("e")[0], got[1]))

    def test_g(self):
        for x in self.values():
            for prec in range(1, 21):
                got = format(x, ".%dg" % prec)
                mant, exp = exact_e(x, prec - 1)
                if -4 <= exp < prec:
                    want = format(decimal.Decimal(mant + "e%d" % exp), "f")
                    if "." in want:
                        want = want.rstrip("0").rstrip(".")
                    self.assertEqual(got, want, (x, prec))
                else:
                    m = mant.rstrip("0").rstrip(".") if "." in mant else mant
                    self.assertEqual(split(got), (m, exp), (x, prec))

    def test_alternate_and_no_type(self):
        self.assertEqual(format(1.5, "#.6g"), "1.50000")
        self.assertEqual(format(1.5, ".6"), "1.5")
        self.assertEqual(format(100.0, ".4"), "100.0")
        self.assertEqual(format(1e16, ".3"), "1e+16")
        self.assertEqual(format(0.0, ".3e"), "0.000e+00")
        self.assertEqual(format(-0.0, "g"), "-0")
        self.assertEqual("%.3g" % 1234.5, "1.23e+03")
        self.assertEqual("%g" % 1.5, "1.5")
        self.assertEqual(f"{2.5:.0e}", "2e+00")
        self.assertEqual(f"{3.5:.0e}", "4e+00")


if __name__ == "__main__":
    unittest.main()

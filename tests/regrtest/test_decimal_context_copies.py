"""`decimal.Context.copy()` and `localcontext()` make independent contexts.

A copy has the source's precision, rounding, exponent limits, capitals,
clamp, flags and traps; changing the copy's values, flags or traps leaves
the source alone and the other way round; the copy's flag and trap
mappings are valid signal dicts of their own; a subclass's copy is a plain
`Context`; and arithmetic under `localcontext()` sees the local settings
and restores the outer ones.
"""

import copy
import decimal
import unittest
from decimal import Context, Decimal, Inexact, Rounded, DivisionByZero


class ContextCopyTests(unittest.TestCase):
    def test_values(self):
        c = Context(prec=12, rounding=decimal.ROUND_DOWN, Emin=-99, Emax=99,
                    capitals=0, clamp=1, traps=[DivisionByZero], flags=[Inexact])
        for d in (c.copy(), copy.copy(c), c.__copy__()):
            self.assertIs(type(d), Context)
            self.assertEqual((d.prec, d.rounding, d.Emin, d.Emax, d.capitals, d.clamp),
                             (12, decimal.ROUND_DOWN, -99, 99, 0, 1))
            self.assertTrue(d.traps[DivisionByZero])
            self.assertFalse(d.traps[Inexact])
            self.assertTrue(d.flags[Inexact])
            self.assertFalse(d.flags[Rounded])
            self.assertEqual(repr(d), repr(c))
            self.assertIsNot(d.flags, c.flags)
            self.assertIsNot(d.traps, c.traps)
            self.assertEqual(len(d.flags), len(c.flags))
            self.assertEqual(dict(d.traps), dict(c.traps))

    def test_independent(self):
        c = Context(prec=10)
        d = c.copy()
        d.prec = 5
        d.traps[Inexact] = True
        d.flags[Rounded] = True
        self.assertEqual(c.prec, 10)
        self.assertFalse(c.traps[Inexact])
        self.assertFalse(c.flags[Rounded])
        c.clear_flags()
        d.clear_traps()
        self.assertTrue(d.flags[Rounded])
        self.assertFalse(d.traps[DivisionByZero])
        self.assertTrue(c.traps[DivisionByZero])
        with self.assertRaises(ValueError):
            d.prec = 0
        with self.assertRaises(KeyError):
            d.traps["nope"] = 1

    def test_subclass(self):
        class MyContext(Context):
            pass

        c = MyContext(prec=7)
        d = c.copy()
        self.assertIs(type(d), Context)
        self.assertEqual(d.prec, 7)

    def test_localcontext(self):
        decimal.setcontext(Context(prec=28))
        outer = decimal.getcontext()
        for _ in range(200):
            with decimal.localcontext() as ctx:
                ctx.prec = 5
                self.assertEqual(str(Decimal(1) / Decimal(3)), "0.33333")
                self.assertIs(decimal.getcontext(), ctx)
            self.assertIs(decimal.getcontext(), outer)
            self.assertEqual(str(Decimal(1) / Decimal(3)), "0.3333333333333333333333333333")
            with decimal.localcontext(prec=3, rounding=decimal.ROUND_UP) as ctx:
                self.assertEqual(str(Decimal("1.001") + 0), "1.01")
        with decimal.localcontext() as ctx:
            ctx.traps[DivisionByZero] = True
            with self.assertRaises(DivisionByZero):
                Decimal(1) / 0
            ctx.traps[DivisionByZero] = False
            self.assertEqual(str(Decimal(1) / 0), "Infinity")
            self.assertTrue(ctx.flags[DivisionByZero])
        self.assertFalse(outer.flags[DivisionByZero])


if __name__ == "__main__":
    unittest.main()

"""Smoke test: decimal / fractions."""

from decimal import Decimal, getcontext

getcontext().prec = 28
a = Decimal("0.1")
b = Decimal("0.2")
assert a + b == Decimal("0.3")
assert (Decimal(1) / Decimal(3)) * 3 != Decimal(1)

from fractions import Fraction

f = Fraction(1, 3)
assert f + Fraction(1, 6) == Fraction(1, 2)
assert Fraction(10, 6) == Fraction(5, 3)
assert float(Fraction(1, 4)) == 0.25
assert str(Fraction(7, 3)) == "7/3"

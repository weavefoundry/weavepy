from decimal import Decimal
from fractions import Fraction

# Decimal arithmetic is exact for base-10 inputs.
print(Decimal("0.1") + Decimal("0.2"))
print(Decimal("0.1") + Decimal("0.2") == Decimal("0.3"))
print(Decimal("3.14159").quantize(Decimal("0.01")))
print(Decimal(2) ** 10)
print(Decimal("100") / Decimal("3"))

# Fractions are rational.
print(Fraction(1, 3) + Fraction(1, 6))
print(Fraction(2, 3) * Fraction(3, 4))
print(Fraction(0.5))
print(Fraction("3/4"))
print(Fraction(0.1).limit_denominator(100))

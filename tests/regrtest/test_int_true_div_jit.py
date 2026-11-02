"""`int / int` in compiled loops gives the correctly rounded float, raises
`ZeroDivisionError`, and keeps a negative zero's sign, for operands of
any size (the in-line division covers operands of at most 53 bits)."""

import math
from fractions import Fraction

PAIRS = [
    (1, 3), (2, 3), (-7, 2), (7, -2), (0, -5), (10**15, 7), (2**53, 3),
    (2**53 + 1, 3), (-(2**53) - 1, 7), (2**60 + 1, 2**60 - 1), (3, 2**54 + 1),
    (2**63 - 1, 9), (-(2**63), 3), (10**30, 7), (1, 10**30),
]


def divide(pairs, n):
    out = []
    for _ in range(n):
        out = []
        for a, b in pairs:
            out.append(a / b)
    return out


def divide_locals(n):
    t = 0.0
    for x in range(n):
        w = x % 17 + 1
        t += (x - w / 2) / w
    return t


got = divide(PAIRS, 3000)
for (a, b), r in zip(PAIRS, got):
    # (A zero quotient takes the divisor's sign, as `0.0 / b` does.)
    want = float(Fraction(a, b)) if a else math.copysign(0.0, b)
    assert r == want and math.copysign(1, r) == math.copysign(1, want), (a, b, r, want)
assert math.copysign(1, got[4]) == -1.0

want = 0.0
for x in range(20000):
    w = x % 17 + 1
    want += float(Fraction(2 * x - w, 2 * w))
assert abs(divide_locals(20000) - want) < 1e-6 * abs(want)


def zero(n):
    for x in range(n):
        x / (x - n + 1)


try:
    zero(5000)
except ZeroDivisionError as e:
    assert str(e) == "division by zero", e
else:
    raise AssertionError("no ZeroDivisionError")

print("ok")

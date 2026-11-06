"""Comparisons of big ints with each other and with machine ints, made
from compiled code (`fractions`' `g == 1` over values past a machine
word), keep CPython's results for every operator and both signs; so do
`math` functions of big ints called from compiled code (`gcd`, `isqrt`)."""
import math
import operator

OPS = [operator.lt, operator.le, operator.eq, operator.ne, operator.gt, operator.ge]


def cmp_all(a, b):
    return (a < b, a <= b, a == b, a != b, a > b, a >= b)


def run(i):
    big = 2 ** 70 + i
    vals = [big, -big, i, -i, 2 ** 63 - 1, -(2 ** 63), 2 ** 63, big * big, 1]
    for a in vals:
        for b in vals:
            assert cmp_all(a, b) == tuple(op(a, b) for op in OPS), (a, b)
    g = (big * 3) // big
    assert g == 3 and not (g == 1) and g != 1
    assert math.gcd(big * 6, big * 4) == big * 2
    assert math.gcd(big, 3) == math.gcd(2 ** 70 + i, 3)
    assert math.isqrt(big * big) == big


for i in range(1500):
    run(i)
print("ok")

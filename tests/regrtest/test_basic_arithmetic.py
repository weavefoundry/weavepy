"""Smoke test: integer/float arithmetic, comparisons, basic ops."""

assert 2 + 2 == 4
assert 2 * 3 == 6
assert 10 / 4 == 2.5
assert 10 // 4 == 2
assert 10 % 3 == 1
assert 2 ** 10 == 1024
assert -5 + 7 == 2

assert 1.0 + 2.0 == 3.0
assert 0.1 + 0.2 != 0.3   # famous float quirk

assert (1 < 2) and (2 < 3)
assert not (3 < 2)
assert 1 == 1.0
assert 1 is not 1.0

assert abs(-7) == 7
assert min(1, 2, 3) == 1
assert max(1, 2, 3) == 3
assert sum([1, 2, 3, 4]) == 10
assert round(3.14, 1) == 3.1
assert round(2.71, 1) == 2.7

assert divmod(17, 5) == (3, 2)
import math
assert math.pow(2, 8) == 256.0

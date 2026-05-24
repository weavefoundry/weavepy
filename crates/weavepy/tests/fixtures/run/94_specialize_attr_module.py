# RFC 0021: LOAD_ATTR_MODULE specialization must return the same
# value before and after the cache warms. We pull a stable
# attribute off `math` in a hot loop and confirm the value never
# wavers.

import math


def calls_math(n):
    total = 0.0
    for _ in range(n):
        total = total + math.pi
    return total


def two_tuple_unpack(n):
    pairs = [(i, i + 1) for i in range(n)]
    out = 0
    for a, b in pairs:
        out = out + a + b
    return out


print(calls_math(50))
print(round(calls_math(10) - 10 * math.pi, 9))
print(two_tuple_unpack(100))

"""Tier-2 integer loops keep CPython's results where an add, a subtract or
a multiply overflows a machine word: the result becomes an exact big int,
both signs, at the first operation that crosses."""


def adds(start, step, n):
    s = start
    i = 0
    while i < n:
        s = s + step
        i = i + 1
    return s


def subs(start, step, n):
    s = start
    i = 0
    while i < n:
        s = s - step
        i = i + 1
    return s


def muls(x, n):
    s = 1
    i = 0
    while i < n:
        s = s * x
        i = i + 1
    return s


big = (1 << 62)
for _ in range(300):
    assert adds(0, 1, 1000) == 1000
    assert subs(0, 3, 1000) == -3000
    assert muls(3, 30) == 3 ** 30
assert adds(big, big // 4, 8) == big + 2 * big
assert subs(-big, big // 4, 8) == -3 * big
assert muls(3, 60) == 3 ** 60
assert muls(-3, 61) == (-3) ** 61
assert adds((1 << 63) - 5, 1, 10) == (1 << 63) + 5
assert subs(-(1 << 63) + 5, 1, 10) == -(1 << 63) - 5
print("ok")

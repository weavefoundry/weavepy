"""Arithmetic sites that the core loop has seen produce or take big ints
(`statistics`' exact sums square numerators past a machine word) keep
CPython's results once compiled: products, sums and differences past the
word, mixed with small ints, in place (`d[k] += n * n`) and not."""
from collections import defaultdict


def acc(p, k, a):
    p[k] += a * a
    return p[k]


def mix(a, b):
    return a * b + a - b


def run(i, p):
    big = 2 ** 70 + i
    small = i - 1500
    want = p[i & 7] + big * big
    assert acc(p, i & 7, big) == want
    want = p[i & 7] + small * small
    assert acc(p, i & 7, small) == want
    assert mix(big, small) == big * small + big - small
    assert mix(small, small) == small * small
    assert mix(2 ** 62, 4) == 2 ** 64 + 2 ** 62 - 4


p = defaultdict(int)
for i in range(3000):
    run(i, p)
total = sum(p.values())
assert total == sum((2 ** 70 + i) ** 2 + (i - 1500) ** 2 for i in range(3000)), total
print("ok")

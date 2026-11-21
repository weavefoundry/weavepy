"""`for i, x in enumerate(seq)` over lists and tuples in compiled code.

The pair is read in place from the source; a list that grows or shrinks
mid-loop, a source iterator also advanced by hand, a `start`, items that
leave the loop's numeric lane, and a counter past the machine word behave
as in CPython. Run it past the compile thresholds, with object and numeric
loop bodies.
"""

import sys


def pairs(xs):
    out = []
    for i, x in enumerate(xs):
        out.append((i, x))
    return out


def weighted(n, xs):
    s = 0
    for _ in range(n):
        for i, x in enumerate(xs):
            s += i * x
    return s


def growing(n):
    xs = [1, 2]
    seen = []
    for i, x in enumerate(xs):
        seen.append(i)
        if len(xs) < n:
            xs.append(x)
    return seen


def shrinking():
    xs = list(range(6))
    seen = []
    for i, x in enumerate(xs):
        seen.append((i, x))
        if xs:
            xs.pop()
    return seen


def shared(xs):
    it = iter(xs)
    out = []
    for i, x in enumerate(it):
        out.append((i, x, next(it, None)))
    return out


N = 3000
for _ in range(N):
    assert pairs([5, 6, 7]) == [(0, 5), (1, 6), (2, 7)]
    assert pairs(("a", "b")) == [(0, "a"), (1, "b")]
    assert pairs([]) == []
    assert growing(5) == [0, 1, 2, 3, 4]
    assert shrinking() == [(0, 0), (1, 1), (2, 2)]
    assert shared([1, 2, 3, 4, 5]) == [(0, 1, 2), (1, 3, 4), (2, 5, None)]
assert weighted(N, list(range(10))) == 285 * N
assert weighted(N, tuple(range(10))) == 285 * N
assert weighted(N, [float(v) for v in range(10)]) == 285.0 * N
assert weighted(1, [1, 2.5, 3]) == 2.5 + 6
assert list(enumerate([7, 8], start=10)) == [(10, 7), (11, 8)]
big = list(enumerate([1, 2], start=sys.maxsize))
assert big == [(sys.maxsize, 1), (sys.maxsize + 1, 2)]
try:
    weighted(1, [1, "x"])
except TypeError:
    pass
else:
    raise AssertionError("int * str in the sum")
print("ok")

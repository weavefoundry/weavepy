"""`s.add(x)` and `s.update(...)` of plain sets in compiled code.

Values that hash and compare natively go straight in: `True` against `1`,
a NaN, strings and bytes, from tuples, lists, sets and frozensets, keep
CPython's results. Values with a Python `__eq__`, `s.update(s)`, other
iterables, a live iterator over the set, and a mutation another operation
deferred behave as in CPython. Run it past the compile threshold.
"""

import math


class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return 1

    def __eq__(self, other):
        return isinstance(other, K) and self.v == other.v


def grow(s, *srcs):
    s.update(*srcs)
    return s


def add_all(s, xs):
    for x in xs:
        s.add(x)
    return s


N = 3000
nan = float("nan")
for i in range(N):
    s = grow(set(), (1, 2), [2, "a"], {b"x"}, frozenset({3.5, None}))
    assert s == {1, 2, "a", b"x", 3.5, None}
    t = grow({1}, (True, 1.0))
    assert t == {1} and type(next(iter(t))) is int
    assert len(grow(set(), (nan, nan))) == 1
    assert grow({1}, {1, 2}, ()) == {1, 2}
    same = {1, 2}
    assert grow(same, same) == {1, 2}
    assert len(grow(set(), [K(1), K(1), K(2)])) == 2
    assert grow(set(), range(3), "ab") == {0, 1, 2, "a", "b"}
    assert add_all(set(), [True, 1, "x", "x", None]) == {True, "x", None}
s = {1, 2}
it = iter(s)
next(it)
grow(s, (3,))
try:
    next(it)
except RuntimeError:
    pass
else:
    raise AssertionError("set grew during iteration")
try:
    grow(set(), [[1]])
except TypeError:
    pass
else:
    raise AssertionError("unhashable")

armed = False


class Bad:
    def __hash__(self):
        return 1

    def __eq__(self, other):
        if armed:
            victim.clear()
        return True


victim = {Bad()}
armed = True
Bad() in victim
armed = False
grow(victim, (7,))
assert victim == {7}, victim
print("ok")

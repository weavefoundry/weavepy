"""Calls of `itemgetter`, `attrgetter` and `partial` instances in a loop.

A call site settled on the instance's native `__call__` runs its fast
half with the instance first; a miss raises as the full call does, and
a different callee at the same site still works. Items are heap objects.
Run it past the compile threshold.
"""

import functools
from operator import attrgetter, itemgetter


class P:
    def __init__(self, x):
        self.x = x


def run(n):
    ig, ag = itemgetter(1), attrgetter("x")
    many = itemgetter(0, 2)
    part = functools.partial(max, 3)
    rows = [("a", [i]) for i in range(4)]
    total = 0
    for i in range(n):
        for f, arg in ((ig, rows[i % 4]), (ag, P([i])), (part, i % 7)):
            r = f(arg)
            total += r[0] if isinstance(r, list) else r
        assert many(("p", "q", "r")) == ("p", "r")
        assert ig({1: "one"}) == "one"
        try:
            ig(("only",))
        except IndexError:
            pass
        else:
            raise AssertionError("short tuple")
        try:
            ig({})
        except KeyError:
            pass
        else:
            raise AssertionError("missing key")
        try:
            ag(1)
        except AttributeError:
            pass
        else:
            raise AssertionError("missing attribute")
    return total


expected = sum((i % 4) + i + max(3, i % 7) for i in range(2000))
assert run(2000) == expected
print("ok")

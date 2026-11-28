"""`list(enumerate(xs))` and `tuple(enumerate(xs))` over lists and tuples.

The pairs come from the rest of the source at once: a `start`, an
enumerate already partly consumed, a source iterator shared with other
code, an empty source, a count past the machine word, a list subclass,
and the exhausted iterators afterwards behave as in CPython.
"""

import sys


class L(list):
    pass


def run(n):
    for _ in range(n):
        xs = [5, 6, 7]
        assert list(enumerate(xs)) == [(0, 5), (1, 6), (2, 7)]
        assert tuple(enumerate(("a", "b"), 3)) == ((3, "a"), (4, "b"))
        e = enumerate(xs, 10)
        assert next(e) == (10, 5)
        assert list(e) == [(11, 6), (12, 7)]
        assert list(e) == [] and next(e, None) is None
        src = iter(xs)
        e2 = enumerate(src)
        assert next(e2) == (0, 5)
        assert list(e2) == [(1, 6), (2, 7)] and next(src, None) is None
        assert list(enumerate([])) == [] and list(enumerate(())) == []
        assert list(enumerate(L([1, 2]))) == [(0, 1), (1, 2)]
        grow = [1]
        assert list(enumerate(grow)) == [(0, 1)]
        grow.append(2)


run(3000)
big = list(enumerate([1, 2], sys.maxsize))
assert big == [(sys.maxsize, 1), (sys.maxsize + 1, 2)]
print("ok")

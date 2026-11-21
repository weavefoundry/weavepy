"""`bisect` over exact lists, the scalar comparisons made natively.

Ints, floats and strings bisect in place; an item whose comparison needs
its `__lt__` continues through it from where the native steps stopped;
`lo`/`hi` past the list raise as CPython's, and `key=` takes the general
path. Run it past the compile threshold.
"""

import bisect


class K:
    calls = 0

    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        K.calls += 1
        o = other.v if isinstance(other, K) else other
        return self.v < o

    def __gt__(self, other):
        K.calls += 1
        o = other.v if isinstance(other, K) else other
        return self.v > o


ints = list(range(0, 200, 2))
mixed = [0, 1.5, 2, 3.5, 4, 5.5]
words = sorted("alpha beta gamma delta epsilon zeta eta theta".split())
objs = [K(i) for i in range(0, 50, 5)]
for i in range(3000):
    assert bisect.bisect(ints, 51) == 26 and bisect.bisect_left(ints, 50) == 25
    assert bisect.bisect_right(ints, 50) == 26
    assert bisect.bisect(mixed, 3) == 3 and bisect.bisect_left(mixed, 2.0) == 2
    assert bisect.bisect(words, "gamma") == words.index("gamma") + 1
    assert bisect.bisect(ints, 51, 10, 40) == 26 and bisect.bisect(ints, -1, 3) == 3
    K.calls = 0
    assert bisect.bisect(objs, 22) == 5 and K.calls > 0
    assert bisect.bisect(ints, 7, key=lambda x: x // 2) == 8
    try:
        bisect.bisect([1, 2], 5, 0, 10)
    except IndexError:
        pass
    else:
        raise AssertionError("no raise")
    xs = []
    bisect.insort(xs, 3); bisect.insort(xs, 1); bisect.insort(xs, 2)
    assert xs == [1, 2, 3]

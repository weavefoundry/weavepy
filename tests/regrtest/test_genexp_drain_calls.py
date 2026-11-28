"""Inlined `any`, `all`, `list` and `tuple` of a generator expression that
calls a function, in compiled code.

The generator's resume drains it into the consumer, as `sum()`'s does,
when its body calls. Deciding items stop the drain where CPython stops;
an item whose truth only `__bool__` can tell goes through it; a raise
from the body reaches the consumer's handler with the items before it
consumed; the collected items keep their order. Items are heap objects.
Run it past the compile threshold.
"""


class Item:
    pass


class Truth:
    def __init__(self, v):
        self.v = v

    def __bool__(self):
        seen.append(self.v)
        return self.v


seen = []
items = [Item() for _ in range(5)]


def ident(x):
    return x


def neg(x):
    return x < 0


def boom(x):
    if x == 3:
        raise ValueError(x)
    return x


def any_neg(xs):
    return any(neg(x) for x in xs)


def all_neg(xs):
    return all(neg(x) for x in xs)


def collect(xs):
    return list(ident(x) for x in xs)


def as_tuple(xs):
    return tuple(ident(x) for x in xs)


def truths(vs):
    return any(Truth(v) for v in vs)


def raising():
    consumed = []
    try:
        return list(consumed.append(x) or boom(x) for x in range(6))
    except ValueError as e:
        return ("raised", e.args[0], consumed)


def stops_early():
    consumed = []
    found = any(consumed.append(x) or neg(x) for x in [1, -2, 3, 4])
    return found, consumed


for _ in range(3000):
    assert any_neg([1, 2, 3]) is False and any_neg([1, -2, 3]) is True
    assert all_neg([-1, -2]) is True and all_neg([-1, 2, -3]) is False
    assert collect(items) == items and as_tuple(items) == tuple(items)
    assert collect([]) == [] and as_tuple(()) == ()
    del seen[:]
    assert truths([False, False, True, False]) is True
    assert seen == [False, False, True], seen
    del seen[:]
    assert truths([False, False]) is False and seen == [False, False]
    assert raising() == ("raised", 3, [0, 1, 2, 3])
    assert stops_early() == (True, [1, -2])

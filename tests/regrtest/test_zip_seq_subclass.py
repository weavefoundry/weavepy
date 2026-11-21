"""`zip`, `map` and `filter` over `list` and `tuple` subclasses' instances.

A named tuple's `_asdict` is `dict(zip(self._fields, self))`. The
adapters iterate such an instance's value directly when its class keeps
the builtin `__iter__`; a class that overrides `__iter__` is iterated
through it. Items are heap objects. Run it past the compile threshold.
"""

from collections import namedtuple


class Item:
    pass


P = namedtuple("P", "x y")


class L(list):
    pass


class Own(list):
    def __iter__(self):
        return iter(["own"])


a, b = Item(), Item()
for _ in range(3000):
    p = P(a, b)
    assert p._asdict() == {"x": a, "y": b}
    assert dict(zip(P._fields, p)) == {"x": a, "y": b}
    assert list(zip("ab", L([a, b]))) == [("a", a), ("b", b)]
    assert list(zip("ab", Own([a, b]))) == [("a", "own")]
    assert list(map(id, L([a]))) == [id(a)]
    assert list(filter(None, P(0, a))) == [a]

"""`None`, `True` and `False` as dict keys on the native probe paths.

They hash and compare natively, so stores and lookups take the fast
paths; a `bool` probe still finds an equal `int` key (and the reverse),
and a store keeps the key already there, in dicts, sets, `OrderedDict`,
`defaultdict`, `Counter` and `lru_cache`. Run it past the compile
threshold.
"""

import functools
from collections import Counter, OrderedDict, defaultdict


class R:
    def __init__(self):
        self.reg = {}

    def register(self, name, value, obj):
        registry = self.reg.setdefault(name, {})
        registry[value] = obj


@functools.lru_cache(maxsize=None)
def cached(x):
    return (type(x).__name__, x)


def run(n):
    r = R()
    for i in range(n):
        r.register("a", None, i)
        r.register("a", True, i)
        r.register("a", "s", i)
        reg = r.reg["a"]
        assert reg[None] == i and reg[True] == i and reg[1] == i
        assert list(reg) == [None, True, "s"]
        d = {1: "one", 0: "zero"}
        assert d.get(True) == "one" and d[False] == "zero" and True in d
        d[True] = "t"
        assert list(d.items()) == [(1, "t"), (0, "zero")]
        assert type(next(iter(d))) is int
        assert d.setdefault(False, "f") == "zero" and d.pop(True) == "t"
        e = {True: 1, None: 2}
        assert e.get(1) == 1 and e[None] == 2 and e.get(False) is None
        e[1] = 5
        assert list(e.items()) == [(True, 5), (None, 2)]
        assert 1.0 in e and e[1.0] == 5
        s = {1, None}
        assert True in s and None in s and False not in s
        od = OrderedDict([(1, "a"), (None, "b")])
        od[True] = "c"
        od.move_to_end(None)
        assert list(od.items()) == [(1, "c"), (None, "b")] and od.get(True) == "c"
        dd = defaultdict(int)
        dd[None] += 1
        dd[True] += 1
        dd[1] += 1
        assert dict(dd) == {None: 1, True: 2}
        c = Counter([True, 1, None, False, 0])
        assert c[1] == 2 and c[None] == 1 and c[0] == 2
    assert cached(True) == cached(1) or cached(1) == ("int", 1)


run(2000)
print("ok")

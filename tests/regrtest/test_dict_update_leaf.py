"""`d.update(other)` and `d.copy()` of plain dicts in compiled code.

A merge from another plain dict whose keys run no code takes the
source's entries in place: an existing key keeps its key object and takes
the new value, new keys append in order, and a live iterator over the
destination notices a new key. Keys with a Python `__eq__`, `d.update(d)`,
keywords, and other mappings behave as in CPython. Run it past the
compile threshold.
"""

import types


class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, K) and self.v == other.v


def merge(d, src):
    d.update(src)
    return d


N = 3000
base = {"a": 1, "b": 2}
one = 1.0
for i in range(N):
    assert merge({}, base) == base
    out = merge({"b": 0, "z": 9}, base)
    assert list(out) == ["b", "z", "a"] and out == {"b": 2, "z": 9, "a": 1}
    d = {one: "x"}
    merge(d, {1: "y"})
    assert d == {1.0: "y"} and type(next(iter(d))) is float
    kd = merge({K(1): 1}, {K(1): 2, K(2): 3})
    assert len(kd) == 2 and kd[K(1)] == 2
    same = {"s": 1}
    assert merge(same, same) == {"s": 1}
    c = base.copy()
    c["a"] = i
    assert base == {"a": 1, "b": 2} and list(c) == ["a", "b"] and c["a"] == i
d = {"a": 1}
d.update({"b": 2}, c=3)
assert d == {"a": 1, "b": 2, "c": 3}
d.update(types.MappingProxyType({"e": 5}))
assert d["e"] == 5
it = iter(d)
next(it)
try:
    for _ in range(N):
        merge(d, {"new": 0})
        next(it)
except RuntimeError:
    pass
else:
    raise AssertionError("a new key during iteration")
print("ok")

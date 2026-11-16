"""`cls(*args, **kwargs)` of a plain class with a Python `__init__`.

`dataclasses.replace` ends in `self.__class__(**changes)`. The core loop
and compiled code bind the mapping's keywords to `__init__` and run it
inline, as a keyword call's construction runs; the mapping is left as it
was, and a binding the fast path can't make (an unexpected or repeated
name) raises the generic call's `TypeError`. Items are heap objects. Run
it past the compile threshold.
"""

import dataclasses


class Item:
    pass


class P:
    def __init__(self, a, b=2, *, c=3):
        self.a, self.b, self.c = a, b, c


class K:
    def __init__(self, a, **rest):
        self.a, self.rest = a, rest


@dataclasses.dataclass
class Pt:
    x: object
    y: int = 0
    tags: list = dataclasses.field(default_factory=list)


def make(cls, *args, **kwargs):
    return cls(*args, **kwargs)


def build(cls, kw):
    return cls(**kw)


item = Item()
for _ in range(3000):
    kw = {"a": item, "c": 5}
    p = build(P, kw)
    assert p.a is item and p.b == 2 and p.c == 5 and kw == {"a": item, "c": 5}
    p = make(P, item, c=7)
    assert p.a is item and p.b == 2 and p.c == 7
    k = build(K, {"a": 1, "z": item})
    assert k.a == 1 and k.rest == {"z": item}
    try:
        build(P, {"a": 1, "zz": 2})
    except TypeError as e:
        assert "zz" in str(e), e
    else:
        raise AssertionError("no TypeError")
    try:
        make(P, 1, a=2)
    except TypeError as e:
        assert "a" in str(e), e
    else:
        raise AssertionError("no TypeError")
    pt = Pt(item, 2)
    q = dataclasses.replace(pt, y=5)
    assert q.x is item and q.y == 5 and q.tags == [] and q is not pt

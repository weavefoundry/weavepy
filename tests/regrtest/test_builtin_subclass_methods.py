"""Builtin methods called on instances of builtin subclasses.

`Counter.most_common` calls `self.items()` on a `dict` subclass's
instance. Compiled code and the core loop call such a method in place
(its body unwraps the payload) instead of leaving for the generic call.
A subclass that overrides the method in Python, a view that follows its
dict, and errors keep their behavior. Items are heap objects. Run it
past the compile threshold.
"""

import collections


class Item:
    pass


class D(dict):
    pass


class OwnItems(dict):
    def items(self):
        return [("own", 1)]


class L(list):
    pass


class S(str):
    pass


a, b = Item(), Item()
c = collections.Counter("aab")
for _ in range(3000):
    d = D(x=a, y=b)
    assert sorted(k for k, _ in d.items()) == ["x", "y"]
    assert list(d.values())[0] is a and list(d.keys()) == ["x", "y"]
    v = d.items()
    d["z"] = 1
    assert len(v) == 3 and ("z", 1) in v
    assert d.get("x") is a and d.get("q", b) is b
    assert d.setdefault("w", a) is a and d.pop("w") is a
    assert OwnItems(k=1).items() == [("own", 1)]
    assert dict(c.items()) == {"a": 2, "b": 1}
    assert c.most_common(1) == [("a", 2)]
    lst = L([a])
    lst.append(b)
    assert lst.pop() is b and lst.index(a) == 0 and len(lst) == 1
    s = S("a-b")
    assert s.upper() == "A-B" and s.split("-") == ["a", "b"]
    try:
        d.pop("missing")
    except KeyError:
        pass
    else:
        raise AssertionError("no KeyError")

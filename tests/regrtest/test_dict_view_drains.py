"""Draining a dict view into a list, tuple or unpacking gives the dict's
entries in order at the time of the drain: keys, values, `(key, value)`
pairs, a dict subclass's own storage, and an `OrderedDict`'s order."""
import collections


class Sub(dict):
    def __iter__(self):
        return iter(["ignored"])


for i in range(3000):
    d = {"b": 1, "a": 2, 3: [4]}
    assert list(d.keys()) == ["b", "a", 3]
    assert list(d.values()) == [1, 2, [4]]
    assert list(d.items()) == [("b", 1), ("a", 2), (3, [4])]
    assert tuple(d.items())[2][1] is d[3]
    assert [*d.values()] == [1, 2, [4]]
    assert sorted(d.items(), key=str) == sorted([("b", 1), ("a", 2), (3, [4])], key=str)
    view = d.items()
    d["c"] = 5
    del d["b"]
    assert list(view) == [("a", 2), (3, [4]), ("c", 5)]
    assert list({}.items()) == []

    s = Sub(x=1, y=2)
    assert list(s.items()) == [("x", 1), ("y", 2)]
    assert list(s.keys()) == ["x", "y"]

    od = collections.OrderedDict([("p", 1), ("q", 2)])
    od.move_to_end("p")
    assert list(od.items()) == [("q", 2), ("p", 1)]
    assert list(od.keys()) == ["q", "p"]
print("ok")

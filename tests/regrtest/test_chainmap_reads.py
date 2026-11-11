"""`ChainMap` reads from compiled loops give what the full lookup gives:
a key in the first map or a later one, a miss's `KeyError`, a subclass's
`__missing__` and its own `__getitem__`, a map that isn't a dict, `maps`
replaced on the instance, and keys that compare equal across types."""

from collections import ChainMap, UserDict


class Defaulting(ChainMap):
    def __missing__(self, key):
        return ("missing", key)


class Upper(ChainMap):
    def __getitem__(self, key):
        return ChainMap.__getitem__(self, key.upper())


def reads(n):
    base = {"a": 1, "b": 2, 3: "three"}
    t = 0
    for i in range(n):
        cm = ChainMap({"b": i}, base)
        t += cm["a"] + cm["b"]
        t += cm[3.0] == "three"
        try:
            cm["zz"]
        except KeyError as e:
            t += e.args == ("zz",)
    return t


for _ in range(3):
    assert reads(5000) == 5000 + sum(range(5000)) + 5000 + 5000


def subclasses(n):
    out = []
    for i in range(n):
        d = Defaulting({"x": i})
        u = Upper({"K": i})
        out.append((d["x"], d["y"], u["k"]))
    return out[-1]


for _ in range(3):
    assert subclasses(3000) == (2999, ("missing", "y"), 2999)


def others(n):
    t = 0
    ud = UserDict({"u": 5})
    for i in range(n):
        cm = ChainMap(ud, {"v": 1})
        t += cm["u"] + cm["v"]
        cm.maps = [{"u": 100}]
        t += cm["u"]
    return t


for _ in range(3):
    assert others(3000) == 3000 * 106
print("ok")

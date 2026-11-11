"""`k in d.keys()` from compiled loops answers as the dict's membership
does: `int` and `str` keys present and absent, `1.0` and `True` against
`int` keys, a view kept in a local, a view of a dict that dies with it, a
dict subclass's view (which keeps its instance alive), keys added and
removed between tests, `values()` and `items()` views, and `len()` of
each view, a subclass's included."""


class D(dict):
    log = []

    def __del__(self):
        D.log.append("del")


def members(n):
    d = {i: str(i) for i in range(0, 20, 2)}
    s = {"a": 1, "b": 2}
    t = 0
    for i in range(n):
        t += (i % 20) in d.keys()
        t += "a" in s.keys()
        t += "z" in s.keys()
        t += 1.0 in {1: 0}.keys()
        t += True in d.keys()
    return t


for _ in range(3):
    assert members(2000) == 1000 + 2000 + 0 + 2000 + 0


def kept(n):
    d = {1: 1}
    ks = d.keys()
    t = 0
    for i in range(n):
        if i == n // 2:
            d[2] = 2
            del d[1]
        t += (1 in ks) + 2 * (2 in ks)
    return t


for _ in range(3):
    assert kept(2000) == 1000 + 2 * 1000


def subclass(n):
    t = 0
    for i in range(n):
        D.log.clear()
        t += 3 in D({3: 1}).keys()
        t += len(D.log)
    return t


for _ in range(3):
    assert subclass(2000) == 2000 * 2


def others(n):
    d = {1: "x", 2: "y"}
    t = 0
    for i in range(n):
        t += "x" in d.values()
        t += (2, "y") in d.items()
        t += 3 in d.keys()
    return t


for _ in range(3):
    assert others(2000) == 4000


def lengths(n):
    d = {1: 1, 2: 2, 3: 3}
    t = 0
    for i in range(n):
        t += len(d.values()) + len(d.keys()) + len(d.items())
        D.log.clear()
        t += len(D({1: 2}).values())
        t += len(D.log)
    return t


for _ in range(3):
    assert lengths(2000) == 2000 * (9 + 1 + 1)
print("ok")

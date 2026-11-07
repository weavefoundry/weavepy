"""Compiled loops keep CPython's results where list and range iterators
step in line and dict lookups by `str` keys probe in line: a list grown or
cleared while it is iterated, an iterator advanced inside its own loop,
ranges with negative and large steps, lookups of equal but distinct
strings, of colliding and missing keys, of `str` subclass keys, of a dict
changed between lookups, and of owned containers and keys."""


class S(str):
    def __hash__(self):
        return 7


def grow(n):
    xs = [0, 1, 2]
    out = []
    for x in xs:
        out.append(x)
        if len(xs) < n:
            xs.append(x + 3)
    return out


def clear():
    xs = list(range(10))
    out = []
    for x in xs:
        out.append(x)
        if x == 3:
            xs.clear()
    return out


def skip():
    it = iter(list(range(10)))
    out = []
    for x in it:
        out.append(x)
        next(it, None)
    return out


def ranges():
    t = []
    for i in range(10, 0, -3):
        t.append(i)
    for i in range(0, 1 << 62, 1 << 60):
        t.append(i)
    for i in range(5, 5):
        t.append(i)
    return t


def lookups(d, keys):
    out = []
    for k in keys:
        try:
            out.append(d[k])
        except KeyError:
            out.append(None)
    return out


def owned(d, a, b):
    t = 0
    for _ in range(100):
        t += len(d[a + b]) + len(dict(d)[a])
    return t


for _ in range(300):
    assert grow(12) == list(range(12))
    assert clear() == [0, 1, 2, 3]
    assert skip() == [0, 2, 4, 6, 8]
    assert ranges() == [10, 7, 4, 1, 0, 1 << 60, 2 << 60, 3 << 60]

keys = ["k%d" % i for i in range(50)]
d = {k: i for i, k in enumerate(keys)}
equal = ["".join(["k", str(i)]) for i in range(50)]
for _ in range(300):
    assert lookups(d, keys) == list(range(50))
    assert lookups(d, equal) == list(range(50))
    assert lookups(d, ["nope", "k1"]) == [None, 1]
d2 = {S("a"): 1, S("b"): 2, "c": 3, 4: 4}
for _ in range(300):
    assert lookups(d2, ["a", "b", "c", 4, S("c"), S("a")]) == [None, None, 3, 4, None, 1]
d3 = dict(d)
for i in range(300):
    d3["k1"] = i
    assert lookups(d3, ["k1"]) == [i]
del d3["k1"]
assert lookups(d3, ["k1", "k2"]) == [None, 2]
d4 = {"ab": "xyz", "a": "q"}
for _ in range(300):
    assert owned(d4, "a", "b") == 400
print("ok")

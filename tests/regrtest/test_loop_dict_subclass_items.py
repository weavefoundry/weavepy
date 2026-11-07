"""Hot loops' item reads and stores on `dict` subclasses keep CPython's
results: a `defaultdict`'s and a `Counter`'s missing keys go through
`__missing__`, present ones read and update in place, and a subclass that
overrides `__getitem__` or `__setitem__` has its method called every
time."""
import collections


def bump(d, n):
    for i in range(n):
        d[i & 63] += 1
    return d


dd = bump(collections.defaultdict(int), 64000)
assert len(dd) == 64 and all(v == 1000 for v in dd.values())
c = bump(collections.Counter(), 64000)
assert c.most_common(1)[0][1] == 1000 and sum(c.values()) == 64000


class Logged(dict):
    def __init__(self):
        super().__init__()
        self.sets = 0

    def __setitem__(self, k, v):
        self.sets += 1
        super().__setitem__(k, v)


class Doubled(dict):
    def __getitem__(self, k):
        return 2 * super().__getitem__(k)


lg = Logged()
for i in range(64):
    lg[i] = 0
bump(lg, 64000)
assert lg.sets == 64 + 64000 and lg[0] == 1000, (lg.sets, lg[0])

db = Doubled((i, 0) for i in range(64))


def reads(d, n):
    t = 0
    for i in range(n):
        t += d[i & 63]
    return t


assert reads(db, 64000) == 0
db[1] = 5
assert reads(db, 6400) == 6400 // 64 * 10


def names(d, words, n):
    for _ in range(n):
        for w in words:
            d[w] = d[w] + len(w)
    return d


nd = names(collections.defaultdict(int), ["a", "bb", "ccc"], 20000)
assert dict(nd) == {"a": 20000, "bb": 40000, "ccc": 60000}
od = collections.OrderedDict((w, 0) for w in ["x", "yy"])
names(od, ["x", "yy"], 20000)
assert list(od.items()) == [("x", 20000), ("yy", 40000)]
print("ok")

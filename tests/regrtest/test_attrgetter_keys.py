"""`hash()` of plain instances, `operator.attrgetter` and `itemgetter`
calls, and `sorted`/`map` keys that run no code give what the full calls
give: identity hashes and overridden `__hash__`, attributes read off plain
instances, properties, methods, class values, dotted names, several names,
named tuples, `__getattr__` fallbacks, the errors for a missing attribute,
and sort keys that raise partway or switch from a pure key to a full one."""

import operator
from collections import namedtuple
from operator import attrgetter, itemgetter


class P:
    kind = "p"

    def __init__(self, x, y):
        self.x = x
        self.y = y

    @property
    def total(self):
        return self.x + self.y

    def neg(self):
        return -self.x


class H:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return self.v * 7


class Fallback:
    def __getattr__(self, name):
        return name.upper()


Pt = namedtuple("Pt", "a b")


def hashes(n):
    out = 0
    for i in range(n):
        p = P(i, 0)
        out += hash(p) == object.__hash__(p)
        out += hash(H(i)) == i * 7
    return out


for _ in range(3):
    assert hashes(300) == 600


def getters(n):
    gx = attrgetter("x")
    gt = attrgetter("total")
    gn = attrgetter("neg")
    gk = attrgetter("kind")
    gxy = attrgetter("x", "y")
    gd = attrgetter("x.real")
    ga = attrgetter("a")
    gf = attrgetter("zz")
    gi = itemgetter(1)
    out = []
    for i in range(n):
        p = P(i, 2)
        out.append(
            (gx(p), gt(p), gn(p)(), gk(p), gxy(p), gd(p), ga(Pt(i, 0)),
             gf(Fallback()), gi([0, i]))
        )
    return out


for _ in range(3):
    got = getters(200)
    assert got == [
        (i, i + 2, -i, "p", (i, 2), i, i, "ZZ", i) for i in range(200)
    ], got[:3]


def misses(n):
    g = attrgetter("missing")
    seen = 0
    for i in range(n):
        try:
            g(P(i, 0))
        except AttributeError as e:
            seen += "missing" in str(e)
        try:
            g(object())
        except AttributeError:
            seen += 1
    return seen


for _ in range(3):
    assert misses(200) == 400


def keys(n):
    ps = [P(i % 7, -i) for i in range(n)]
    a = sorted(ps, key=attrgetter("x"))
    b = sorted(ps, key=attrgetter("total"))
    c = sorted(ps, key=lambda p: p.y, reverse=True)
    d = list(map(attrgetter("x"), ps))
    e = sorted([(i % 3, i) for i in range(n)], key=itemgetter(0))
    return a, b, c, d, e


for _ in range(3):
    a, b, c, d, e = keys(100)
    assert [p.x for p in a] == sorted(i % 7 for i in range(100))
    assert [(p.x, p.y) for p in a[:2]] == [(0, 0), (0, -7)]
    assert [p.total for p in b] == sorted(p.total for p in b)
    assert [p.y for p in c] == list(range(0, -100, -1))
    assert d == [i % 7 for i in range(100)]
    assert [t[0] for t in e] == sorted(i % 3 for i in range(100))
    assert e[:2] == [(0, 0), (0, 3)]


class Odd:
    def __init__(self, x):
        self.x = x


def partial_keys(n):
    items = [Odd(i) for i in range(n)]
    items[n // 2] = object()
    try:
        sorted(items, key=attrgetter("x"))
    except AttributeError:
        return "raised"
    return "sorted"


for _ in range(3):
    assert partial_keys(50) == "raised"


def mixed(n):
    items = [Odd(i) for i in range(n)]
    items[3] = P(3, 0)
    items[3].x = 3
    return [o.x for o in sorted(items, key=attrgetter("x"))]


for _ in range(3):
    assert mixed(20) == list(range(20))


assert operator.attrgetter("x")(P(9, 1)) == 9
print("ok")

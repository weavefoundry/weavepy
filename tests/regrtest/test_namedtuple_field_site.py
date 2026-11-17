"""Named tuple field reads in compiled code.

A field read answers from its site's stamp while the class's version
holds; a subclass with an instance `__dict__` (the field descriptor still
wins), a subclass that overrides a field with a property, a class changed
mid-loop, and a site that sees several named tuple classes behave as in
CPython. Run it past the compile threshold.
"""

from collections import namedtuple

P = namedtuple("P", "x y label")


class Sub(P):
    pass


class Prop(P):
    @property
    def x(self):
        return -1


def total(n, p):
    t = 0
    for _ in range(n):
        t += p.x + p.y
    return t


def labels(n, ps):
    out = []
    for i in range(n):
        out.append(ps[i % len(ps)].label)
    return out


N = 3000
assert total(N, P(1, 2, "a")) == 3 * N
s = Sub(3, 4, "s")
s.x_shadow = 9
s.__dict__["x"] = 100
assert total(N, s) == 7 * N
assert total(N, Prop(5, 6, "p")) == 5 * N
assert labels(N, [P(0, 0, "a"), Sub(0, 0, "b"), Prop(0, 0, "c")])[:3] == ["a", "b", "c"]
q = P(7, 8, "q")
assert total(N, q) == 15 * N
P.y = property(lambda self: 1000)
assert total(10, q) == 1007 * 10
del P.y
try:
    total(1, q)
except AttributeError:
    pass
else:
    raise AssertionError("deleted field")
print("ok")

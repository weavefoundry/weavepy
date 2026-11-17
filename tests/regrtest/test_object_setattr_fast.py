"""`object.__setattr__` on plain instances, its compiled fast half included.

A store into a plain instance's own dict or a writable `__slots__` member
takes the native half; properties and other data descriptors, read-only
and missing slots, exceptions' `args`, a published `__dict__`, and a
replaced value's finalizer behave as in CPython. Run it past the compile
threshold.
"""

import dataclasses


@dataclasses.dataclass(frozen=True)
class Frozen:
    a: int
    b: str


class Plain:
    pass


class Slotted:
    __slots__ = ("x", "y")


class WithProp:
    def __init__(self):
        self.log = []

    @property
    def p(self):
        return self.log

    @p.setter
    def p(self, v):
        self.log.append(v)


class Desc:
    def __set_name__(self, owner, name):
        self.name = "_" + name

    def __get__(self, obj, cls=None):
        return getattr(obj, self.name, None)

    def __set__(self, obj, v):
        object.__setattr__(obj, self.name, v * 2)


class WithDesc:
    d = Desc()


class D:
    log = []

    def __init__(self, n):
        self.n = n

    def __del__(self):
        D.log.append(self.n)


class E(Exception):
    pass


def run(n):
    for i in range(n):
        f = Frozen(i, "x")
        assert (f.a, f.b) == (i, "x")
        try:
            f.a = 1
        except dataclasses.FrozenInstanceError:
            pass
        else:
            raise AssertionError("frozen")
        object.__setattr__(f, "a", i + 1)
        assert f.a == i + 1 and f == Frozen(i + 1, "x")
        p = Plain()
        object.__setattr__(p, "v", i)
        object.__setattr__(p, "v", [i])
        assert p.v == [i] and p.__dict__ == {"v": [i]}
        s = Slotted()
        object.__setattr__(s, "x", i)
        assert s.x == i
        try:
            object.__setattr__(s, "z", 1)
        except AttributeError:
            pass
        else:
            raise AssertionError("no slot z")
        w = WithProp()
        object.__setattr__(w, "p", i)
        assert w.log == [i]
        wd = WithDesc()
        object.__setattr__(wd, "d", i)
        assert wd.d == 2 * i
        e = E(1)
        object.__setattr__(e, "args", [2, 3])
        assert e.args == (2, 3)
        pd = Plain()
        dd = pd.__dict__
        object.__setattr__(pd, "k", i)
        assert dd["k"] == i
        h = Plain()
        object.__setattr__(h, "held", D(i))
        D.log.clear()
        object.__setattr__(h, "held", None)
        assert D.log == [i], D.log
        o = Plain()
        o.__setattr__("m", i)
        assert o.m == i


run(3000)
print("ok")

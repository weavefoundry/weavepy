"""Constructors whose `__init__` is a leaf of stores keep CPython's results
when the stores run as native code: a field read back after its store, a
store that replaces an earlier one, an `__init__` that raises after some
stores, a `__slots__` class, many fields, a store into another object
the constructor is passed, and a class whose `__init__` changes after
warm-up."""
import math


class P:
    def __init__(self, i):
        self.x = math.sin(i)
        self.y = math.cos(i) * 3.0
        self.z = (self.x * self.x) / 2.0
        self.x = self.x + 1.0


class S:
    __slots__ = ("a", "b")

    def __init__(self, a):
        self.a = a
        self.b = self.a * 2


class Many:
    def __init__(self, v):
        self.f0 = v
        self.f1 = v + 1
        self.f2 = v + 2
        self.f3 = v + 3
        self.f4 = v + 4
        self.f5 = v + 5
        self.f6 = v + 6
        self.f7 = v + 7
        self.f8 = self.f7 + self.f0


class Box:
    pass


class Into:
    def __init__(self, box, v):
        self.v = v
        box.seen = v


class Bad:
    def __init__(self, v):
        self.v = v
        self.w = 10 // v


def make(n):
    out = []
    for i in range(n):
        out.append((P(i), S(i), Many(i)))
    return out


for _ in range(3):
    pts = make(3000)
p, s, m = pts[1234]
assert p.x == math.sin(1234) + 1.0 and p.y == math.cos(1234) * 3.0
assert p.z == math.sin(1234) ** 2 / 2.0
assert (s.a, s.b) == (1234, 2468) and m.f8 == 1234 + 7 + 1234
assert vars(p) == {"x": p.x, "y": p.y, "z": p.z}
box = Box()
for i in range(3000):
    assert Into(box, i).v == i and box.seen == i
errs = 0
for i in range(-1500, 1500):
    try:
        assert Bad(i).w == 10 // i
    except ZeroDivisionError:
        errs += 1
assert errs == 1


def init2(self, i):
    self.x = -i
    self.y = self.z = 0


P.__init__ = init2
assert make(5)[3][0].x == -3
print("ok")

"""Attribute reads and stores of instance fields that compiled code makes
in line keep CPython's results across what the guards must catch: a class
changed after the code compiled, an instance whose `__dict__` was handed
out, a subclass with its own layout, `__slots__` members (set and unset),
an attribute deleted, a heap value stored into an instance that was
holding only scalars, the last reference to a receiver, and a property."""
import gc
import weakref


class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def bump(self):
        self.x += 1
        self.y = self.x * 2
        return self.x + self.y


class Q(P):
    def __init__(self, x, y):
        self.z = 0
        super().__init__(x, y)


class S:
    __slots__ = ("a", "b")

    def __init__(self, a):
        self.a = a

    def step(self):
        self.a += 1
        return self.a


class Prop:
    def __init__(self):
        self._v = 0

    @property
    def v(self):
        return self._v

    @v.setter
    def v(self, value):
        self._v = value * 10


class Holder:
    def __init__(self):
        self.n = 0

    def put(self, value):
        self.n = 1
        self.n = value
        return self.n


def make():
    return P(1, 2)


def run(i):
    p = P(i, 0)
    assert p.bump() == (i + 1) * 3
    q = Q(i, 0)
    assert q.bump() == (i + 1) * 3 and q.z == 0
    s = S(i)
    assert s.step() == i + 1
    try:
        s.b
    except AttributeError:
        pass
    else:
        raise AssertionError("an unset slot read")
    s.b = i
    assert s.b == i
    pr = Prop()
    pr.v = i
    assert pr.v == i * 10
    h = Holder()
    lst = [h]
    assert h.put(lst) is lst
    make().x = 5
    return p


ps = [run(i) for i in range(3000)]

# A field read after the instance's dict went out.
p = P(1, 2)
d = vars(p)
d["x"] = 41
assert p.bump() == 42 + 84
# The class changed after compiling: a class attribute shadowed by data.
P.y = property(lambda self: 7, lambda self, v: None)
p2 = P(3, 4)
assert p2.bump() == 4 + 7
del P.y
p3 = P(5, 6)
assert p3.bump() == 6 + 12
# A deleted attribute.
p4 = P(1, 1)
del p4.y
assert p4.bump() == 2 + 4
# A cycle closed by a store into an instance that held only scalars.
h = Holder()
h.put(h)
r = weakref.ref(h)
del h
gc.collect()
assert r() is None
print("ok")

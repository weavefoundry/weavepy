"""A class's attribute read through an instance from compiled code sees
every change that would change the answer: the class attribute rebound or
deleted, an instance that sets its own value (in its values, through
`__dict__`, through `vars()`), an instance of another class, a subclass
that overrides it, a value of another kind, and a class whose attribute
becomes a descriptor."""


class C:
    K = 5

    def __init__(self, x):
        self.x = x


class D(C):
    K = 7


class E(C):
    pass


def read(o, n):
    total = 0
    for _ in range(n):
        total += o.K
    return total


def warm():
    o = C(1)
    for _ in range(5):
        assert read(o, 400) == 2000


warm()

o = C(1)
assert read(o, 100) == 500

# The class attribute rebound, then deleted.
C.K = 6
assert read(o, 100) == 600
C.K = 5
assert read(o, 100) == 500

# The instance's own value, set as an attribute (values laid over the
# class's names grow past the proof's limit).
p = C(2)
p.K = 1
assert read(p, 100) == 100
assert read(o, 100) == 500

# Through `__dict__` and `vars()`.
q = C(3)
q.__dict__["K"] = 2
assert read(q, 100) == 200
r = C(4)
vars(r)["K"] = 3
assert read(r, 100) == 300

# An instance with no values at all.
bare = C.__new__(C)
assert read(bare, 100) == 500

# Subclasses: overriding and inheriting.
assert read(D(1), 100) == 700
assert read(E(1), 100) == 500
E.K = 9
assert read(E(1), 100) == 900
assert read(o, 100) == 500

# A value of another kind.
C.K = 2.5
assert read(o, 4) == 10.0
C.K = 5

# A descriptor on the class.


class Desc:
    def __get__(self, inst, owner):
        return 11


C.K = Desc()
assert read(o, 10) == 110
C.K = 5
assert read(o, 10) == 50


# Deleted from the class: the instance's own or AttributeError.
del C.K
try:
    read(o, 1)
except AttributeError:
    pass
else:
    raise AssertionError("expected AttributeError")
assert read(p, 10) == 10
C.K = 5
assert read(o, 10) == 50

# An instance whose values are mutated during the loop.


class F:
    K = 1

    def __init__(self):
        self.a = 0


def read_mut(o, n):
    total = 0
    for i in range(n):
        total += o.K
        if i == n // 2:
            o.K = 100
    return total


for _ in range(5):
    f = F()
    assert read_mut(f, 100) == 51 + 49 * 100, read_mut(F(), 100)

print("ok")

"""Constructing classes with keyword arguments keeps the generic binder's
semantics: defaults, keyword-only and variadic `__init__`s, binder error
messages, `__init__` returning non-None, a changed `__init__` or
`__defaults__`, metaclass `__call__`, abstract classes, custom `__new__`,
and argument lifetimes."""

import abc
import gc


class Point:
    def __init__(self, x, y=0, z=0):
        self.x = x
        self.y = y
        self.z = z


def make_point():
    return Point(1, z=3)


for _ in range(300):
    p = make_point()
    assert (p.x, p.y, p.z) == (1, 0, 3)


class KwOnly:
    def __init__(self, a, *, b, c=5):
        self.v = (a, b, c)


class Var:
    def __init__(self, *args, **kw):
        self.v = (args, sorted(kw.items()))


class VarSelf:
    def __init__(self, a, *rest, **kw):
        self.v = (a, rest, sorted(kw.items()))


for _ in range(300):
    assert KwOnly(1, b=2).v == (1, 2, 5)
    assert KwOnly(a=1, b=2, c=3).v == (1, 2, 3)
    assert Var(1, b=2).v[1] == [("b", 2)]
    assert len(Var(1, b=2).v[0]) == 1
    assert VarSelf(1, 2, k=3).v == (1, (2,), [("k", 3)])


def expect_type_error(fn, msg):
    for _ in range(100):
        try:
            fn()
        except TypeError as e:
            assert str(e) == msg, (str(e), msg)
        else:
            raise AssertionError("expected TypeError: " + msg)


expect_type_error(lambda: Point(1, w=2), "Point.__init__() got an unexpected keyword argument 'w'")
expect_type_error(lambda: Point(1, x=2), "Point.__init__() got multiple values for argument 'x'")
expect_type_error(lambda: Point(self=1, x=2), "Point.__init__() got multiple values for argument 'self'")
expect_type_error(lambda: Point(y=2), "Point.__init__() missing 1 required positional argument: 'x'")
expect_type_error(lambda: KwOnly(1, c=2), "KwOnly.__init__() missing 1 required keyword-only argument: 'b'")


class ReturnsValue:
    def __init__(self, a=0):
        self.a = a
        return a


expect_type_error(lambda: ReturnsValue(a=1), "__init__() should return None, not 'int'")
for _ in range(50):
    assert ReturnsValue(a=None).a is None


# `__init__` and its defaults changed after the site warmed up.
class Changing:
    def __init__(self, a, b=1):
        self.v = ("first", a, b)


def make_changing():
    return Changing(1, b=2)


for _ in range(300):
    assert make_changing().v == ("first", 1, 2)


def second(self, a, b=10, c=20):
    self.v = ("second", a, b, c)


Changing.__init__ = second
for _ in range(100):
    assert make_changing().v == ("second", 1, 2, 20)
second.__defaults__ = (100, 200)
for _ in range(100):
    assert make_changing().v == ("second", 1, 2, 200)
second.__defaults__ = None
expect_type_error(make_changing, "second() missing 1 required positional argument: 'c'")


# A metaclass `__call__` intercepts construction.
class Meta(type):
    def __call__(cls, *args, **kw):
        obj = super().__call__(*args, **kw)
        obj.via_meta = True
        return obj


class WithMeta(metaclass=Meta):
    def __init__(self, a, b=0):
        self.v = (a, b)


for _ in range(200):
    w = WithMeta(1, b=2)
    assert w.v == (1, 2) and w.via_meta


# Abstract classes refuse instantiation whatever the arguments.
class Base(abc.ABC):
    @abc.abstractmethod
    def f(self):
        pass

    def __init__(self, a=0):
        self.a = a


expect_type_error(
    lambda: Base(a=1),
    "Can't instantiate abstract class Base without an implementation for abstract method 'f'",
)


# A custom `__new__` sees the keywords too.
class WithNew:
    def __new__(cls, *args, **kw):
        obj = super().__new__(cls)
        obj.seen = sorted(kw)
        return obj

    def __init__(self, a, b=0):
        self.v = (a, b)


for _ in range(200):
    w = WithNew(1, b=2)
    assert w.v == (1, 2) and w.seen == ["b"]


# Subclass `__init__` chaining with keywords.
class Child(Point):
    def __init__(self, x, **kw):
        super().__init__(x, **kw)
        self.child = True


for _ in range(200):
    c = Child(1, y=2)
    assert (c.x, c.y, c.z, c.child) == (1, 2, 0, True)


# A keyword argument's last reference dies with the construction.
class Tracked:
    alive = 0

    def __init__(self):
        Tracked.alive += 1

    def __del__(self):
        Tracked.alive -= 1


class Holder:
    def __init__(self, a, b=None):
        self.a = a


def make_holder():
    Holder(1, b=Tracked())
    return Tracked.alive


gc.disable()
try:
    for _ in range(300):
        assert make_holder() == 0
finally:
    gc.enable()

print("ok")

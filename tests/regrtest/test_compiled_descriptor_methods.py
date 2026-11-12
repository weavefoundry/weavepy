"""Compiled code's method loads of `staticmethod`s and `classmethod`s
through an instance keep CPython's results: the class (a subclass's own
for an inherited `classmethod`) or nothing is bound, an instance
attribute of the same name shadows the descriptor, and a class whose
descriptor is replaced or removed loads the new attribute."""


class Base:
    @classmethod
    def make(cls):
        return cls

    @staticmethod
    def twice(x):
        return 2 * x


class Sub(Base):
    pass


def call_both(o, n):
    t = 0
    for i in range(n):
        t += o.twice(i) if o.make() is type(o) else -1
    return t


for o in (Base(), Sub()):
    for _ in range(3):
        assert call_both(o, 3000) == 2 * sum(range(3000))

mixed = [Base(), Sub(), Base(), Sub()]
for _ in range(3000):
    for o in mixed:
        assert o.make() is type(o)

shadow = Base()
for _ in range(3000):
    assert shadow.twice(3) == 6
shadow.twice = lambda x: "own"
assert shadow.twice(3) == "own"
shadow.make = lambda: "own make"
assert shadow.make() == "own make"


class Swap:
    @staticmethod
    def f():
        return 1


s = Swap()


def get(o):
    return o.f()


for _ in range(3000):
    assert get(s) == 1
Swap.f = staticmethod(lambda: 2)
assert get(s) == 2
Swap.f = classmethod(lambda cls: cls)
assert get(s) is Swap
del Swap.f
try:
    get(s)
except AttributeError:
    pass
else:
    raise AssertionError("no AttributeError")
print("ok")

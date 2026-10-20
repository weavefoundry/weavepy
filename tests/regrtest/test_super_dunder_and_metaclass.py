"""Zero-argument `super()` resolving dunders past built-in bases, and in
metaclass methods whose receiver is a class: the same attribute CPython
finds, bound the same way, through class changes after warm-up.

(The module never names `super` at top level, which would make the
compiler use the generic `super()` call instead of `LOAD_SUPER_ATTR`.)"""


class D(dict):
    def __setitem__(self, k, v):
        super().__setitem__(k, v * 2)

    def __getitem__(self, k):
        return super().__getitem__(k) + 1

    def __len__(self):
        return super().__len__() + 100

    def __eq__(self, other):
        return super().__eq__(other)

    def __repr__(self):
        return "D" + super().__repr__()

    def __contains__(self, k):
        return super().__contains__(k)


d = D()
for i in range(300):
    d["k"] = i
    assert d["k"] == 2 * i + 1
    assert len(d) == 101
    assert d == {"k": 2 * i}
    assert repr(d) == "D{'k': %d}" % (2 * i)
    assert "k" in d and "z" not in d


class L(list):
    def __getitem__(self, i):
        return super().__getitem__(i) * 10

    def __iter__(self):
        return super().__iter__()


lst = L([1, 2, 3])
for _ in range(300):
    assert lst[1] == 20
    assert list(iter(lst)) == [1, 2, 3]


# Dunders that only `object` provides.
class P:
    def __init__(self):
        super().__setattr__("x", 1)

    def __setattr__(self, k, v):
        super().__setattr__(k, v + 1)

    def __getattribute__(self, k):
        return super().__getattribute__(k)

    def __eq__(self, other):
        return super().__eq__(other)

    def __hash__(self):
        return super().__hash__()

    def __format__(self, spec):
        return "P" + super().__format__("")[:0]


for _ in range(300):
    p = P()
    assert p.x == 1
    p.y = 5
    assert p.y == 6
    assert (p == p) is True and (p == 1) is False
    assert p.__eq__(1) is NotImplemented
    assert hash(p) == object.__hash__(p)
    assert format(p, "") == "P"


# A user class between the subclass and the built-in base.
class Mid(dict):
    def __setitem__(self, k, v):
        super().__setitem__(k, ("mid", v))


class Leaf(Mid):
    def __setitem__(self, k, v):
        super().__setitem__(k, ("leaf", v))


lf = Leaf()
for i in range(300):
    lf["a"] = i
    assert dict.__getitem__(lf, "a") == ("mid", ("leaf", i))


# Metaclass methods: `super()` with a class receiver.
log = []


class Meta(type):
    def __setattr__(cls, k, v):
        log.append(("set", k))
        super().__setattr__(k, v)

    def __delattr__(cls, k):
        log.append(("del", k))
        super().__delattr__(k)

    def __getattribute__(cls, k):
        return super().__getattribute__(k)

    def describe(cls):
        return super().__repr__()

    def __call__(cls, *args):
        return super().__call__(*args)


class C(metaclass=Meta):
    def __init__(self, a=0):
        self.a = a


for i in range(300):
    C.attr = i
    assert C.attr == i
    del C.attr
    assert not hasattr(C, "attr")
    assert C.describe().startswith("<class ")
    assert C(i).a == i
assert log.count(("set", "attr")) == 300 and log.count(("del", "attr")) == 300
try:
    del C.missing
except AttributeError as e:
    assert "missing" in str(e)
else:
    raise AssertionError("expected AttributeError")


# A metaclass hierarchy: super() from the derived metaclass reaches the
# base metaclass's Python method.
class BaseMeta(type):
    def tag(cls):
        return "base:" + cls.__name__


class DerivedMeta(BaseMeta):
    def tag(cls):
        return "derived>" + super().tag()


class T(metaclass=DerivedMeta):
    pass


for _ in range(300):
    assert T.tag() == "derived>base:T"


# Class methods and `__new__`: a class receiver that subclasses the
# `super()` class.
class Base:
    @classmethod
    def make(cls):
        return ("base", cls.__name__)

    def __new__(cls, *args):
        return super().__new__(cls)


class Sub(Base):
    @classmethod
    def make(cls):
        return ("sub",) + super().make()

    def __new__(cls, *args):
        return super().__new__(cls, *args)


for _ in range(300):
    assert Sub.make() == ("sub", "base", "Sub")
    assert type(Sub(1)) is Sub


# `super().__class__` is the proxy's.
class S:
    def cls_of_super(self):
        return super().__class__


for _ in range(100):
    assert S().cls_of_super().__name__ == "super"


# The resolution follows a class change made after warm-up.
class Base2(dict):
    pass


class Child2(Base2):
    def __setitem__(self, k, v):
        super().__setitem__(k, v)


c2 = Child2()
for i in range(300):
    c2["x"] = i
assert dict.__getitem__(c2, "x") == 299


def base2_setitem(self, k, v):
    dict.__setitem__(self, k, ("patched", v))


Base2.__setitem__ = base2_setitem
for i in range(50):
    c2["x"] = i
    assert dict.__getitem__(c2, "x") == ("patched", i)
del Base2.__setitem__
c2["x"] = 7
assert dict.__getitem__(c2, "x") == 7

print("ok")

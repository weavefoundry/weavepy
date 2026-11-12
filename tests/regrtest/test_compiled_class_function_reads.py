"""A compiled read of a function off a class (`cls.__enter__`,
`T.method`) remembers the function without owning it: a replaced or
deleted function is seen at once, dies when the class lets it go (a weak
reference to it clears), and dunder names the class defines read as the
class's own, while `type`'s own attributes still read as `type`'s."""

import gc
import weakref


class T:
    def __enter__(self):
        return "enter"

    def method(self):
        return "method"

    def __init__(self):
        self.x = 1


def read_enter(cls, n):
    out = None
    for _ in range(n):
        out = cls.__enter__
    return out


def read_method(cls, n):
    out = None
    for _ in range(n):
        out = cls.method
    return out


def read_local_global(n):
    out = None
    for _ in range(n):
        out = T.method
    return out


for _ in range(3):
    assert read_enter(T, 500) is T.__dict__["__enter__"]
    assert read_method(T, 500) is T.__dict__["method"]
    assert read_local_global(500) is T.__dict__["method"]

# A replaced function: the new one at once, the old one freed.
old = T.__dict__["method"]
ref = weakref.ref(old)
del old


def method2(self):
    return "method2"


T.method = method2
assert read_method(T, 500) is method2
assert read_local_global(500) is method2
gc.collect()
assert ref() is None, "the replaced function is still alive"

# A deleted function: the base's, then an AttributeError.
class Base:
    def method(self):
        return "base"


class Sub(Base):
    def method(self):
        return "sub"


for _ in range(3):
    assert read_method(Sub, 500) is Sub.__dict__["method"]
del Sub.method
assert read_method(Sub, 500) is Base.__dict__["method"]
del Base.method
try:
    read_method(Sub, 10)
except AttributeError:
    pass
else:
    raise AssertionError("no AttributeError")

# A base's function replaced: its subclasses see the new one.
class A:
    def method(self):
        return "a"


class B(A):
    pass


for _ in range(3):
    assert read_method(B, 500) is A.__dict__["method"]


def method3(self):
    return "a3"


A.method = method3
assert read_method(B, 500) is method3

# A dunder the class defines reads as its own; `type`'s read as `type`'s.
def read_init(cls, n):
    out = None
    for _ in range(n):
        out = cls.__init__
    return out


def read_name(cls, n):
    out = None
    for _ in range(n):
        out = cls.__name__
    return out


assert read_init(T, 500) is T.__dict__["__init__"]
assert read_name(T, 500) == "T"
T.__name__ = "Renamed"
assert read_name(T, 500) == "Renamed"

# A class's own entry under a name `type` serves itself reads as `type`'s.
class Sig:
    __text_signature__ = "(a, /, b)"

    def __name__(self):
        return "not the name"


def read_text_signature(cls, n):
    out = None
    for _ in range(n):
        out = cls.__text_signature__
    return out


def read_dunder_name(cls, n):
    out = None
    for _ in range(n):
        out = cls.__name__
    return out


for _ in range(3):
    assert read_text_signature(Sig, 500) is None, read_text_signature(Sig, 500)
    assert read_dunder_name(Sig, 500) == "Sig"

# A metaclass with its own `__getattribute__` keeps the say.
class Meta(type):
    def __getattribute__(cls, name):
        if name == "method":
            return "from meta"
        return super().__getattribute__(name)


class M(metaclass=Meta):
    def method(self):
        return "m"


assert read_method(M, 500) == "from meta"

# A static method and a class method keep their descriptors' results.
class S:
    @staticmethod
    def st():
        return "st"

    @classmethod
    def cm(cls):
        return cls


def read_st(n):
    out = None
    for _ in range(n):
        out = S.st
    return out


def read_cm(n):
    out = None
    for _ in range(n):
        out = S.cm
    return out


assert read_st(500)() == "st"
assert read_cm(500)() is S

# An `Enum` member reads as itself; a descriptor instance runs its `__get__`.
from enum import Enum


class Kind(Enum):
    A = 1
    B = 2
    C = 3


def read_member(n):
    out = None
    for _ in range(n):
        out = Kind.C
    return out


for _ in range(3):
    assert read_member(500) is Kind.C
    assert read_member(500).value == 3


class Counting:
    calls = 0

    def __get__(self, obj, owner):
        Counting.calls += 1
        return ("got", owner.__name__)


class HasDesc:
    attr = Counting()


def read_attr(cls, n):
    out = None
    for _ in range(n):
        out = cls.attr
    return out


assert read_attr(HasDesc, 500) == ("got", "HasDesc")
assert Counting.calls == 500, Counting.calls


class Plain:
    pass


class Holder:
    pass


Holder.attr = Holder()
first = Holder.attr
for _ in range(3):
    assert read_attr(Holder, 500) is first
Holder.attr = Holder()
assert read_attr(Holder, 500) is Holder.__dict__["attr"]
ref = weakref.ref(first)
del first
gc.collect()
assert ref() is None, "a replaced class attribute is still alive"

print("ok")

"""Compiled code's attribute reads on a class itself keep CPython's results:
a constant, a docstring (the class's own, `None` when it has none even if
a base has one), under `type` and under `ABCMeta`, a value the class
reassigns, and a metaclass whose property or own attribute takes the
name."""
import abc


class Flags:
    FROZEN = 1
    LABEL = "flags"


class Base(abc.ABC):
    """Base doc."""

    LIMIT = 10


class Child(Base):
    pass


class Documented(Base):
    """Child doc."""


def read(cls, n):
    t = 0
    for _ in range(n):
        t += cls.FROZEN if cls is Flags else cls.LIMIT
    return t


def docs(cls, n):
    out = None
    for _ in range(n):
        out = cls.__doc__
    return out


assert read(Flags, 3000) == 3000 and read(Child, 3000) == 30000
assert docs(Base, 3000) == "Base doc."
assert docs(Documented, 3000) == "Child doc."
assert docs(Child, 3000) is None and Child.__doc__ is None
Flags.FROZEN = 2
assert read(Flags, 10) == 20
Base.LIMIT = 3
assert read(Child, 10) == 30
Documented.__doc__ = "changed"
assert docs(Documented, 10) == "changed"


class Meta(type):
    @property
    def LABEL(cls):
        return "from meta"


class Shadow(metaclass=Meta):
    LABEL = "own"


def label(cls, n):
    out = None
    for _ in range(n):
        out = cls.LABEL
    return out


assert label(Flags, 3000) == "flags"
assert label(Shadow, 3000) == "from meta"


class Meta2(type):
    LABEL = "meta atom"


class Plain2(metaclass=Meta2):
    pass


class Own2(metaclass=Meta2):
    LABEL = "own atom"


assert label(Own2, 3000) == "own atom"
assert label(Plain2, 3000) == "meta atom"
print("ok")

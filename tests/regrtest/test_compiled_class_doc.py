"""A compiled read of a class's `__doc__` (`type(self).__doc__`, as
`contextlib.contextmanager` reads it at every construction) gives the
class's own current docstring: after it's reassigned (to None too), for a
subclass, through a metaclass that has a docstring of its own, and for a
descriptor stored as the docstring."""

import abc


class Plain:
    pass


class Documented:
    "first"


class Child(Documented):
    pass


class WithMeta(metaclass=abc.ABCMeta):
    "meta-made"


def read(objs, n):
    out = None
    for _ in range(n):
        out = [type(o).__doc__ for o in objs]
    return out


def read_cls(classes, n):
    out = None
    for _ in range(n):
        out = [c.__doc__ for c in classes]
    return out


objs = [Plain(), Documented(), Child(), WithMeta()]
classes = [Plain, Documented, Child, WithMeta]
expect = [None, "first", None, "meta-made"]
for _ in range(3):
    assert read(objs, 300) == expect, read(objs, 300)
    assert read_cls(classes, 300) == expect

# A reassigned docstring is read afresh; a subclass keeps its own.
Documented.__doc__ = "second"
assert read(objs, 300) == [None, "second", None, "meta-made"]
assert read_cls(classes, 300) == [None, "second", None, "meta-made"]
Child.__doc__ = "child"
assert read(objs, 300)[2] == "child"
WithMeta.__doc__ = "changed"
assert read(objs, 300)[3] == "changed"

# A docstring set to None reads as None.
Documented.__doc__ = None
assert read(objs, 300)[1] is None
assert read_cls(classes, 300)[1] is None


# A descriptor stored as the docstring is called, every time.
class DocDescr:
    def __init__(self):
        self.calls = 0

    def __get__(self, obj, owner):
        self.calls += 1
        return "described %d" % self.calls


class Described:
    __doc__ = DocDescr()


got = read([Described()], 50)
assert got[0].startswith("described ")
assert Described.__dict__["__doc__"].calls >= 50


# A metaclass whose `__doc__` is a property answers for its classes.
class Meta(type):
    @property
    def __doc__(cls):
        return "from the metaclass"


class Governed(metaclass=Meta):
    "own"


assert read([Governed()], 300) == ["from the metaclass"]
assert read_cls([Governed], 300) == ["from the metaclass"]

# contextmanager's constructions keep the generator function's docstring.
import contextlib


@contextlib.contextmanager
def managed():
    "the managed doc"
    yield 1


@contextlib.contextmanager
def undocumented():
    yield 2


for _ in range(500):
    with managed() as a, undocumented() as b:
        assert (a, b) == (1, 2)
assert managed().__doc__ == "the managed doc"
assert undocumented().__doc__ == type(undocumented()).__doc__

print("ok")

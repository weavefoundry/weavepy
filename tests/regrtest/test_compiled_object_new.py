"""A user `__new__` that allocates with `object.__new__(cls)` (as
`pathlib`'s paths do), called from compiled code directly or through
`type(self)(*segments)`, keeps CPython's results: the instance of the
class called, its `__init__` run once with the arguments, and the errors
`object.__new__` raises for a class that can't take its allocation. An
unset `__slots__` member read in a `try` (pathlib's cached-parse idiom)
raises the `AttributeError` its handler catches."""


INITS = [0]


class Path:
    __slots__ = ("raw", "_parsed")

    def __new__(cls, *args):
        return object.__new__(cls)

    def __init__(self, *args):
        INITS[0] += 1
        self.raw = "/".join(args)

    def with_segments(self, *segments):
        return type(self)(*segments)

    def __truediv__(self, other):
        return self.with_segments(self.raw, other)

    @property
    def parsed(self):
        try:
            return self._parsed
        except AttributeError:
            self._parsed = self.raw.split("/")
            return self._parsed


class Sub(Path):
    __slots__ = ()


class Abstract:
    def __new__(cls):
        return object.__new__(cls)


import abc


class Ab(abc.ABC):
    def __new__(cls):
        return object.__new__(cls)

    @abc.abstractmethod
    def f(self):
        pass


def run(i):
    p = Path("a") / ("b%d" % i)
    assert type(p) is Path and p.raw == "a/b%d" % i
    assert p.parsed == ["a", "b%d" % i] and p.parsed is p.parsed
    s = Sub("x") / "y"
    assert type(s) is Sub and s.parsed == ["x", "y"]
    assert type(Abstract()) is Abstract


for i in range(3000):
    run(i)
assert INITS[0] == 3000 * 4, INITS[0]
try:
    Ab()
except TypeError:
    pass
else:
    raise AssertionError("an abstract class was instantiated")
print("ok")

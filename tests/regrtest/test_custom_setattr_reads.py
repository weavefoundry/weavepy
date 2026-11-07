"""Attribute reads on instances of classes that define `__setattr__` (a
frozen dataclass's, say) keep their semantics once they're cached."""
import dataclasses
from dataclasses import dataclass


@dataclass(frozen=True)
class Frozen:
    a: int
    b: str = "b"


class Doubling:
    log = []

    def __setattr__(self, name, value):
        Doubling.log.append(name)
        object.__setattr__(self, name, value * 2)


class Fixed:
    def __getattribute__(self, name):
        return 42


class Descr:
    def __get__(self, obj, owner=None):
        return "descr"

    def __set__(self, obj, value):
        raise AttributeError("read-only")


class WithData:
    d = Descr()

    def __setattr__(self, name, value):
        object.__setattr__(self, name, value)


def read_a(x):
    return x.a


def read_d(x):
    return x.d


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    f = Frozen(i)
    assert read_a(f) == i and f.b == "b"
    try:
        f.a = 5
    except dataclasses.FrozenInstanceError:
        pass
    else:
        raise AssertionError("a frozen dataclass took a store")
    assert read_a(f) == i
    patched = i > 2000
    s = Doubling()
    s.a = i
    assert read_a(s) == (-1 if patched else 2 * i) and Doubling.log[-1] == "a"
    s.a = i + 1
    assert read_a(s) == (-1 if patched else 2 * (i + 1))
    assert read_a(Fixed()) == 42
    w = WithData()
    w.__dict__["d"] = "shadowed"
    assert read_d(w) == "descr"
    if i == 2000:
        # A `__getattribute__` added later takes over the cached reads.
        Doubling.__getattribute__ = lambda self, name: -1
    assert read_a(s) == (-1 if i >= 2000 else 2 * (i + 1)), read_a(s)
    assert object.__getattribute__(s, "a") == 2 * (i + 1)
print("custom __setattr__ reads: ok")

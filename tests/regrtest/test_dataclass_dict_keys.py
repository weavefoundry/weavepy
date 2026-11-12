"""Dicts keyed by frozen dataclasses and other instances whose `__hash__`
and `__eq__` are small Python functions answer as the full probe does when
the probe key is an equal but distinct instance: reads, `in`, stores,
`setdefault`, `pop` and `del`, unequal keys that share a hash, an `int`
stored under the same hash, a subclass instance, an `__eq__` that returns
`NotImplemented` or a non-`bool`, and one that raises."""

from dataclasses import dataclass


@dataclass(frozen=True)
class K:
    a: int
    b: str


@dataclass(frozen=True)
class SubK(K):
    pass


class Collide:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return 5

    def __eq__(self, other):
        if other.__class__ is self.__class__:
            return self.v == other.v
        return NotImplemented


class Weird:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return self.v

    def __eq__(self, other):
        if other.__class__ is self.__class__:
            return self.v if self.v else 0
        return NotImplemented


class Raises:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return 1

    def __eq__(self, other):
        return 1 // (self.v - other.v) == 7


def reads(n):
    d = {K(i, str(i)): i for i in range(10)}
    out = 0
    for r in range(n):
        for i in range(10):
            k = K(i, str(i))
            out += d[k]
            out += k in d
            out += d.get(K(i, "x"), 100)
            out += K(i + 10, str(i)) in d
    return out


for _ in range(3):
    assert reads(50) == 50 * (45 + 10 + 1000)


def writes(n):
    d = {}
    for r in range(n):
        for i in range(5):
            d[K(i, "s")] = r
            d.setdefault(K(i, "s"), -1)
            d.setdefault(K(i, "t"), r)
    assert len(d) == 10
    assert d[K(0, "s")] == n - 1 and d[K(0, "t")] == 0
    for i in range(5):
        assert d.pop(K(i, "t")) == 0
        del d[K(i, "s")]
    return len(d)


for _ in range(3):
    assert writes(40) == 0


def collisions(n):
    d = {Collide(i): i for i in range(6)}
    d[5] = "int"
    out = 0
    for r in range(n):
        for i in range(6):
            out += d[Collide(i)]
        out += Collide(9) in d
        out += d[5] == "int"
    return out


for _ in range(3):
    assert collisions(50) == 50 * (15 + 0 + 1)


def subclass(n):
    d = {K(1, "a"): "base"}
    out = 0
    for r in range(n):
        out += d.get(SubK(1, "a"), "miss") == "miss"
        out += d[K(1, "a")] == "base"
    return out


for _ in range(3):
    assert subclass(50) == 50 * (1 + 1)


def weird(n):
    d = {Weird(0): "zero", Weird(3): "three"}
    out = 0
    for r in range(n):
        out += Weird(0) in d
        out += d[Weird(3)] == "three"
    return out


for _ in range(3):
    assert weird(50) == 50


def raising(n):
    d = {Raises(0): "r"}
    seen = 0
    for r in range(n):
        try:
            d[Raises(0)]
        except ZeroDivisionError:
            seen += 1
        assert d.get(Raises(1)) is None
    return seen


for _ in range(3):
    assert raising(50) == 50
print("ok")

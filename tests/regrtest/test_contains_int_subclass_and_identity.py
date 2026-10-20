"""`x in (a, b)` with `int`-subclass members (IntEnum, IntFlag) compares
by value under `int.__eq__`, functions and classes by identity, and any
user `__eq__` (on either side, or on a metaclass) still runs."""

import enum


class Color(enum.IntEnum):
    RED = 1
    GREEN = 2


class Perm(enum.IntFlag):
    R = 4
    W = 2


for _ in range(300):
    assert Color.RED in (Color.GREEN, Color.RED)
    assert Color.RED in (2, 1)
    assert 2 in (Color.RED, Color.GREEN)
    assert True in (Color.RED,)
    assert Color.RED not in (Color.GREEN, 3)
    assert Perm.R in (Perm.W, 4)
    assert (Perm.R | Perm.W) in (6,)
    assert 1.0 in (Color.RED,)
    assert Color.GREEN in [0, 2.0]


class Loud(int):
    calls = 0

    def __eq__(self, other):
        Loud.calls += 1
        return True

    __hash__ = int.__hash__


for _ in range(100):
    assert 5 in (Loud(1),)
    assert Loud(1) in (7,)
assert Loud.calls == 200


def f():
    pass


def g():
    pass


class K:
    pass


for _ in range(300):
    assert f in (None, g, f)
    assert f not in (None, g, "f", 1, K)
    assert K in (int, K)
    assert K not in (int, f, None)
    assert f in (len, None, f)
    assert f not in (len, print)
    assert len in (f, len)
    assert K not in (len,)


class Wild:
    calls = 0

    def __eq__(self, other):
        Wild.calls += 1
        return other is f


w = Wild()
for _ in range(100):
    assert f in (w,)
    assert K not in (w,)
assert Wild.calls == 200


class Meta(type):
    def __eq__(cls, other):
        return other == "anything"

    __hash__ = type.__hash__


class M(metaclass=Meta):
    pass


for _ in range(100):
    assert "anything" in (M,)
    assert M in ("anything",)

print("ok")

"""Operators of builtin subclasses with Python methods, and tuple loops.

Compiled code calls an `int` subclass's own Python `__or__` (`IntFlag`'s)
directly; a subclass that keeps `int`'s operator must still compute as an
`int`, and a reflected operand's method must still win where CPython lets
it. A loop over a fresh tuple of objects (`for flag in self, other`)
releases the tuple in place when its items live on, and must still run a
finalizer an item owes when the tuple was its last owner. Run it past the
compile threshold.
"""

import enum


class Color(enum.IntFlag):
    R = 1
    G = 2
    B = 4


class Plain(int):
    pass


class Loud(int):
    def __or__(self, other):
        return ("or", int(self), int(other))

    def __ror__(self, other):
        return ("ror", int(other), int(self))


class Sub(Loud):
    def __ror__(self, other):
        return ("sub-ror", int(other), int(self))


freed = []


class Dies:
    def __del__(self):
        freed.append(1)


def pair(a, b):
    n = 0
    for x in a, b:
        if x is not None:
            n += 1
    return n


for _ in range(3000):
    assert Color.R | Color.G == Color(3)
    assert (Color.R | Color.B) & Color.B == Color.B
    assert Plain(5) | Plain(2) == 7 and type(Plain(5) | Plain(2)) is int
    assert Loud(1) | Loud(2) == ("or", 1, 2)
    assert Loud(1) | 4 == ("or", 1, 4)
    assert Loud(1) | Sub(2) == ("sub-ror", 1, 2)
    assert pair(Color.R, Color.G) == 2

freed.clear()
for _ in range(500):
    assert pair(Dies(), Dies()) == 2
assert len(freed) == 1000, len(freed)

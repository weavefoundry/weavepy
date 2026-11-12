"""An `int` subclass that keeps `int`'s hash, equality and ordering (as
`re`'s opcodes do) hashes, compares and orders as its value in compiled
code: `in` a set or dict, `min` and `max` (with big ints too). Subclasses
that override any of those, and `IntEnum` (whose `Enum` defines
`__hash__`), keep their own behavior."""
import enum


class Named(int):
    def __new__(cls, value, name):
        self = super().__new__(cls, value)
        self.name = name
        return self

    def __repr__(self):
        return self.name


class LoudEq(int):
    def __eq__(self, other):
        return int(self) == int(other)

    __hash__ = int.__hash__


class OwnHash(int):
    def __hash__(self):
        return 7


class Backwards(int):
    def __lt__(self, other):
        return int(self) > int(other)

    def __gt__(self, other):
        return int(self) < int(other)


class Color(enum.IntEnum):
    RED = 1
    GREEN = 2


A, B, C = Named(1, "A"), Named(2, "B"), Named(3, "C")
codes = frozenset({A, B})
table = {A: "a", B: "b", 5: "five"}
BIG = 1 << 64


def check(i):
    assert A in codes and B in codes and C not in codes
    assert 1 in codes and 3 not in codes
    assert Named(2, "B2") in codes
    assert table[A] == "a" and table[2] == "b" and table[Named(5, "F")] == "five"
    assert (A, 2) in {(1, B)}
    assert (A, (B, None)) in {(1, (2, None))}
    assert min(A, B) is A and max(A, B) is B
    assert min(C, BIG) is C and max(C, BIG) == BIG
    assert min(Named(1, "x"), 1) == 1 and type(min(Named(1, "x"), 1)) is Named
    assert min(Backwards(1), Backwards(2)) == 2
    assert min(Color.GREEN, Color.RED) is Color.RED
    assert Color.RED in {1} and Color.GREEN in {Color.GREEN}
    # (Hashed 7 by its own `__hash__`: a plain 5 hashes elsewhere.)
    assert OwnHash(1) not in {1} and OwnHash(9) not in {OwnHash(5)}
    assert OwnHash(5) in {OwnHash(5)} and 5 not in {OwnHash(5)}
    assert LoudEq(4) in {4} and LoudEq(5) not in {4}


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    check(i)
print("ok")

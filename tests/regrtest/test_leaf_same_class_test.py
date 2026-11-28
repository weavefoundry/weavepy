"""`other.__class__ is self.__class__` in compiled `__eq__` bodies.

A leaf body's two `__class__` reads and the `is` between them run as one
test of the instances' classes. It must agree with the reads it replaces:
a dataclass's `__eq__` against an equal, an unequal, a subclass's and a
foreign operand; a class whose `__class__` is a property; `is not`; an
instance whose `__class__` was reassigned; and non-instance operands.
"""

import dataclasses
import unittest


@dataclasses.dataclass(frozen=True)
class D:
    x: int
    y: int


@dataclasses.dataclass(frozen=True)
class E(D):
    pass


class Masked:
    @property
    def __class__(self):
        return int

    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return other.__class__ is self.__class__

    __hash__ = object.__hash__


class Pair:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        if other.__class__ is not self.__class__:
            return NotImplemented
        return self.v == other.v

    __hash__ = object.__hash__


class Other:
    def __init__(self, v):
        self.v = v


class SameClassTests(unittest.TestCase):
    def test_dataclass_eq(self):
        a, b, c, e = D(1, 2), D(1, 2), D(1, 3), E(1, 2)
        d = {a: "a"}
        for _ in range(300):
            self.assertEqual(d[b], "a")
            self.assertTrue(a == b)
            self.assertFalse(a == c)
            self.assertFalse(a == e)
            self.assertFalse(a == (1, 2))
            self.assertFalse(a == 5)
            self.assertNotIn(e, d)

    def test_property_class(self):
        m = Masked(1)
        for _ in range(300):
            self.assertTrue(m == 5)
            self.assertTrue(m == Masked(2))
            self.assertFalse(m == "x")

    def test_is_not_and_reassigned(self):
        p, q = Pair(1), Pair(1)
        for _ in range(300):
            self.assertTrue(p == q)
            self.assertFalse(p == Other(1))
            self.assertFalse(p == 1)
        r = Pair(1)
        r.__class__ = Other
        for _ in range(300):
            self.assertFalse(p == r)
        r.__class__ = Pair
        for _ in range(300):
            self.assertTrue(p == r)


if __name__ == "__main__":
    unittest.main()

"""`obj.__class__` read inside an in-place evaluated method.

A dataclass's `__eq__` compares `other.__class__ is self.__class__`; the
read answers the instance's class where `object`'s descriptor does, and a
class that defines its own `__class__`, or an instance whose class was
reassigned, still reads as Python reads it.
"""

import dataclasses
import unittest

N = 30000


@dataclasses.dataclass
class P:
    a: int
    b: int


class Q(P):
    pass


class Fake:
    @property
    def __class__(self):
        return P


def same_class(x, y):
    return x.__class__ is y.__class__


class LeafClassReadTests(unittest.TestCase):
    def test_dataclass_eq(self):
        p1, p2, q = P(1, 2), P(1, 2), Q(1, 2)
        for _ in range(N):
            a = p1 == p2
            b = p1 == q
            c = p1 == P(1, 3)
        self.assertEqual((a, b, c), (True, False, False))

    def test_overridden_and_reassigned(self):
        class A:
            pass

        class B:
            pass

        x, y = A(), A()
        f = Fake()
        out = []
        for i in range(N):
            if i == N // 2:
                y.__class__ = B
            out.append((same_class(x, y), same_class(f, P(0, 0))))
        self.assertEqual(out[0], (True, True))
        self.assertEqual(out[-1], (False, True))


if __name__ == "__main__":
    unittest.main()

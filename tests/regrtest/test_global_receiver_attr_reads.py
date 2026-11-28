"""Attribute reads of a receiver that isn't a local (`p.x` of a global).

Compiled code reads a `__slots__` member, a class's plain value through an
instance, and a named tuple's field of a receiver on the stack as it reads
a local's. Changes to the class, an instance entry shadowing a class
value, a deleted slot, and receivers of other classes at the same site
read as CPython reads them.
"""

import unittest
from collections import namedtuple

N = 30000


class S:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


class O:
    K = 5

    def __init__(self):
        self.v = 1


NT = namedtuple("NT", "a b c")

p = S(1, 2)
o = O()
nt = NT(1, 2, 3)


def read_slot():
    return p.x


def read_class():
    return o.K


def read_field():
    return nt.b


class GlobalReceiverReadTests(unittest.TestCase):
    def test_slot(self):
        global p
        out = []
        for i in range(N):
            if i == N // 3:
                p.x = 7
            if i == N // 2:
                p = S(9, 9)
            out.append(read_slot())
        self.assertEqual((out[0], out[N // 3], out[-1]), (1, 7, 9))
        del p.x
        with self.assertRaises(AttributeError):
            read_slot()
        p = S(1, 2)

    def test_class_value(self):
        out = []
        for i in range(N):
            if i == N // 4:
                O.K = 6
            if i == N // 2:
                o.K = "own"
            if i == 3 * N // 4:
                del o.K
            out.append(read_class())
        self.assertEqual(out[0], 5)
        self.assertEqual(out[N // 4], 6)
        self.assertEqual(out[N // 2], "own")
        self.assertEqual(out[-1], 6)
        O.K = 5

    def test_class_value_becomes_property(self):
        class C:
            K = 1

        global c_inst
        c_inst = C()

        def rd():
            return c_inst.K

        out = []
        for i in range(N):
            if i == N // 2:
                C.K = property(lambda self: "prop")
            out.append(rd())
        self.assertEqual((out[0], out[-1]), (1, "prop"))

    def test_named_tuple_field(self):
        global nt
        out = []
        for i in range(N):
            if i == N // 2:
                nt = NT(4, 5, 6)
            out.append(read_field())
        self.assertEqual((out[0], out[-1]), (2, 5))

    def test_mixed_receivers(self):
        class A:
            __slots__ = ("x",)

            def __init__(self):
                self.x = "a"

        class B:
            def __init__(self):
                self.x = "b"

        global mixed
        items = [A(), B(), S(3, 4)]
        out = []
        for i in range(N):
            mixed = items[i % 3]
            out.append(mixed_read())
        self.assertEqual(out[:3], ["a", "b", 3])
        self.assertEqual(out[-3:], ["a", "b", 3])


def mixed_read():
    return mixed.x


if __name__ == "__main__":
    unittest.main()

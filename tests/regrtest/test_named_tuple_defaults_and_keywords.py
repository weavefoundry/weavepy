"""Named tuples built with defaults filled in or with keyword arguments.

The construction lanes build the tuple directly from the positionals, the
keywords placed by field name, and the generated `__new__`'s defaults as
they stand; every call that binds otherwise must still raise as CPython's.
"""

import collections
import typing
import unittest


P = collections.namedtuple("P", "x y z", defaults=(10, [20]))


class T(typing.NamedTuple):
    a: int
    b: str = "b"
    c: tuple = ()


class Sub(P):
    __slots__ = ()

    def total(self):
        return self.x + self.y


class NamedTupleConstructionTests(unittest.TestCase):
    ROUNDS = 4000

    def test_defaults(self):
        for _ in range(self.ROUNDS):
            self.assertEqual(P(1), (1, 10, [20]))
            self.assertEqual(P(1, 2), (1, 2, [20]))
            self.assertEqual(T(1), (1, "b", ()))
            self.assertEqual(T(1, "x"), (1, "x", ()))
        self.assertIs(P(1).z, P(2).z)
        self.assertIs(type(P(1)), P)
        self.assertIs(type(Sub(1)), Sub)
        self.assertEqual(Sub(1).total(), 11)

    def test_keywords(self):
        for _ in range(self.ROUNDS):
            self.assertEqual(P(x=1, y=2, z=3), (1, 2, 3))
            self.assertEqual(P(1, z=3), (1, 10, 3))
            self.assertEqual(P(z=3, x=1), (1, 10, 3))
            self.assertEqual(T(c=(1,), a=0), (0, "b", (1,)))
            self.assertEqual(Sub(y=5, x=1).total(), 6)

    def test_binding_errors(self):
        for _ in range(3):
            with self.assertRaises(TypeError):
                P()
            with self.assertRaises(TypeError):
                P(y=1)
            with self.assertRaises(TypeError):
                P(1, x=2)
            with self.assertRaises(TypeError):
                P(1, w=2)
            with self.assertRaises(TypeError):
                P(1, 2, 3, 4)
            with self.assertRaises(TypeError):
                T()
            with self.assertRaises(TypeError):
                P(1, **{"y": 1, "q": 2})

    def test_defaults_rebound(self):
        Q = collections.namedtuple("Q", "a b", defaults=(1,))
        for _ in range(self.ROUNDS):
            self.assertEqual(Q(0), (0, 1))
        Q.__new__.__defaults__ = (7, 8)
        self.assertEqual(Q(), (7, 8))
        self.assertEqual(Q(0), (0, 8))
        Q.__new__.__defaults__ = None
        with self.assertRaises(TypeError):
            Q(0)
        self.assertEqual(Q(0, b=2), (0, 2))

    def test_new_replaced(self):
        R = collections.namedtuple("R", "a b", defaults=(1,))
        for _ in range(self.ROUNDS):
            self.assertEqual(R(a=0), (0, 1))

        def new(cls, a, b=5):
            return tuple.__new__(cls, (a * 2, b))

        R.__new__ = new
        self.assertEqual(R(1), (2, 5))
        self.assertEqual(R(a=1, b=0), (2, 0))

    def test_star_calls(self):
        args = (1,)
        kw = {"z": 3}
        for _ in range(self.ROUNDS):
            self.assertEqual(P(*args, **kw), (1, 10, 3))
            self.assertEqual(P(**{"x": 1}), (1, 10, [20]))


if __name__ == "__main__":
    unittest.main()

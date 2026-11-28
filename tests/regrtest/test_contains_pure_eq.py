"""`x in d`, `x in s` and `d.get(x)` of an instance whose class's Python
`__hash__` and `__eq__` evaluate frameless settle an equal but distinct
stored key in place; the answers, `not in`, hash collisions between
unequal keys, stored keys of other classes and kinds, an `__eq__` with
effects (called once per comparison, as CPython calls it), and an
`__eq__` that raises match the general lookup."""

import unittest


class P:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __hash__(self):
        return hash((self.x, self.y))

    def __eq__(self, o):
        return isinstance(o, P) and self.x == o.x and self.y == o.y


class Collide:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return 11

    def __eq__(self, o):
        return isinstance(o, Collide) and self.v == o.v


class Counted:
    eqs = 0

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, o):
        Counted.eqs += 1
        return isinstance(o, Counted) and self.v == o.v


class Boom:
    def __hash__(self):
        return 5

    def __eq__(self, o):
        raise ValueError("eq")


class ContainsPureEq(unittest.TestCase):
    def test_dict_and_sets(self):
        d = {P(i, -i): i for i in range(20)}
        s = set(d)
        fs = frozenset(d)
        for _ in range(200):
            for i in range(25):
                k = P(i, -i)
                self.assertEqual(k in d, i < 20)
                self.assertEqual(k in s, i < 20)
                self.assertEqual(k in fs, i < 20)
                self.assertEqual(k not in d, i >= 20)
                self.assertEqual(d.get(k), i if i < 20 else None)
                self.assertEqual(d.get(k, "dflt"), i if i < 20 else "dflt")

    def test_collisions(self):
        d = {Collide(i): i for i in range(6)}
        s = set(d)
        for _ in range(300):
            self.assertIn(Collide(4), d)
            self.assertNotIn(Collide(9), d)
            self.assertIn(Collide(0), s)
            self.assertNotIn(Collide(-1), s)
            self.assertEqual(d.get(Collide(5)), 5)
            self.assertIsNone(d.get(Collide(7)))

    def test_mixed_stored_keys(self):
        d = {P(1, 2): "p", hash((1, 2)): "int", "s": 1}
        s = {P(1, 2), hash((1, 2))}
        for _ in range(300):
            self.assertIn(P(1, 2), d)
            self.assertEqual(d.get(P(1, 2)), "p")
            self.assertIn(P(1, 2), s)
            self.assertNotIn(P(2, 1), d)
            self.assertIn(hash((1, 2)), d)

    def test_eq_with_effects(self):
        d = {Counted(i): i for i in range(5)}
        s = set(d)
        Counted.eqs = 0
        for _ in range(300):
            self.assertIn(Counted(3), d)
            self.assertIn(Counted(2), s)
            self.assertEqual(d.get(Counted(1)), 1)
        self.assertEqual(Counted.eqs, 900)

    def test_eq_raises(self):
        b1 = Boom()
        d = {b1: 1}
        s = {b1}
        for _ in range(300):
            self.assertIn(b1, d)
            with self.assertRaises(ValueError):
                Boom() in d
            with self.assertRaises(ValueError):
                Boom() in s
            with self.assertRaises(ValueError):
                d.get(Boom())


if __name__ == "__main__":
    unittest.main()

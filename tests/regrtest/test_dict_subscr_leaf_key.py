"""Compiled `d[k]` of an instance whose class's Python `__hash__` (and a
stored key's `__eq__`) evaluate frameless reads the value in place; the
values, equal but distinct keys, a missing key's `KeyError` (its args the
key itself), hash collisions, stored keys of other kinds, temporaries as
keys, `__eq__` with effects, and an `__eq__` that raises match the
general subscript."""

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
        return 3

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
        return 9

    def __eq__(self, o):
        raise ValueError("eq")


class DictSubscrLeafKey(unittest.TestCase):
    def test_values(self):
        keys = [P(i, i * 2) for i in range(30)]
        d = {k: i for i, k in enumerate(keys)}
        for _ in range(200):
            for i, k in enumerate(keys):
                self.assertEqual(d[k], i)
                self.assertEqual(d[P(i, i * 2)], i)

    def test_missing(self):
        d = {P(1, 1): "a"}
        for _ in range(300):
            k = P(2, 2)
            with self.assertRaises(KeyError) as cm:
                d[k]
            self.assertIs(cm.exception.args[0], k)

    def test_collisions_and_mixed(self):
        d = {Collide(i): i for i in range(5)}
        d[3] = "int"
        d[hash((7, 7))] = "h"
        d[P(7, 7)] = "p"
        for _ in range(300):
            self.assertEqual(d[Collide(4)], 4)
            self.assertEqual(d[3], "int")
            self.assertEqual(d[P(7, 7)], "p")
            with self.assertRaises(KeyError):
                d[Collide(8)]

    def test_eq_with_effects_and_raises(self):
        d = {Counted(i): i for i in range(4)}
        Counted.eqs = 0
        for _ in range(300):
            self.assertEqual(d[Counted(2)], 2)
        self.assertEqual(Counted.eqs, 300)
        b = Boom()
        e = {b: 1}
        for _ in range(300):
            self.assertEqual(e[b], 1)
            with self.assertRaises(ValueError):
                e[Boom()]


if __name__ == "__main__":
    unittest.main()

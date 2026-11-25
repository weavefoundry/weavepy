"""Stores through property setters that check their value before storing
(an effect leaf with a raise path), from compiled loops: the value stored
and read back, the raise path's error with nothing stored, a setter that
stores two attributes, and a setter replaced partway, each matching
CPython."""

import unittest


class Temp:
    def __init__(self):
        self._c = 0

    @property
    def c(self):
        return self._c

    @c.setter
    def c(self, v):
        if v < -273:
            raise ValueError(v)
        self._c = v


class Pair:
    def __init__(self):
        self.a = self.b = 0

    @property
    def both(self):
        return (self.a, self.b)

    @both.setter
    def both(self, v):
        if v is None:
            raise TypeError("none")
        self.a = v
        self.b = v + 1


def store(n, t):
    for i in range(n):
        t.c = i
    return t.c


class SetterTests(unittest.TestCase):
    def test_store(self):
        for n in (1, 400):
            self.assertEqual(store(n, Temp()), n - 1)

    def test_raise_stores_nothing(self):
        t = Temp()
        store(400, t)
        with self.assertRaises(ValueError):
            for i in range(300):
                t.c = -300 if i == 299 else i
        self.assertEqual(t.c, 298)

    def test_two_stores(self):
        p = Pair()
        for i in range(400):
            p.both = i
        self.assertEqual(p.both, (399, 400))
        with self.assertRaises(TypeError):
            p.both = None
        self.assertEqual(p.both, (399, 400))

    def test_replaced(self):
        class T(Temp):
            pass

        t = T()
        store(400, t)
        T.c = property(lambda self: self._c, lambda self, v: setattr(self, "_c", -v))
        self.assertEqual(store(300, t), -299)


if __name__ == "__main__":
    unittest.main()

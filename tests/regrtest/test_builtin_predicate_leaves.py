"""`id`, `callable`, `issubclass` and `hasattr` called in place.

The answers must match the full calls: identities stay stable, a class's
`__call__` makes its instances callable, a metaclass's
`__subclasscheck__` and a class's `__getattr__` are still consulted, and
argument errors still raise.
"""

import abc
import unittest


class Plain:
    attr = 1


class WithCall:
    def __call__(self):
        return 1


class Lazy:
    def __getattr__(self, name):
        return name


class Meta(type):
    def __subclasscheck__(cls, sub):
        return True


class Anything(metaclass=Meta):
    pass


class BuiltinPredicateLeafTest(unittest.TestCase):
    def test_id(self):
        o = Plain()
        first = id(o)
        for _ in range(2000):
            self.assertEqual(id(o), first)
        with self.assertRaises(TypeError):
            id()

    def test_callable(self):
        p, c = Plain(), WithCall()
        for _ in range(2000):
            self.assertFalse(callable(p))
            self.assertTrue(callable(c))
            self.assertTrue(callable(Plain))
            self.assertFalse(callable(3))

    def test_issubclass(self):
        for _ in range(2000):
            self.assertTrue(issubclass(bool, int))
            self.assertTrue(issubclass(Plain, (int, object)))
            self.assertFalse(issubclass(int, Plain))
            self.assertTrue(issubclass(int, Anything))
            self.assertTrue(issubclass(list, abc.ABC) is False)
        with self.assertRaises(TypeError):
            issubclass(1, int)

    def test_hasattr(self):
        p, lazy = Plain(), Lazy()
        for _ in range(2000):
            self.assertTrue(hasattr(p, "attr"))
            self.assertFalse(hasattr(p, "missing"))
            self.assertTrue(hasattr(lazy, "anything"))
        with self.assertRaises(TypeError):
            hasattr(p, 1)


if __name__ == "__main__":
    unittest.main()

"""`isinstance`/`issubclass` with a class-like instance as the classinfo.

A plain function `__instancecheck__`/`__subclasscheck__` on the
instance's class (the `typing` aliases') is called directly; descriptor
hooks, errors, and hooks replaced on the class behave as before.
"""

import typing
import unittest

N = 3000


class Plain:
    def __instancecheck__(self, obj):
        return obj == 1

    def __subclasscheck__(self, cls):
        return cls is int


class Static:
    @staticmethod
    def __instancecheck__(obj):
        return obj == 2


class Raises:
    def __instancecheck__(self, obj):
        raise KeyError(obj)


class InstanceClassinfoTests(unittest.TestCase):
    def test_plain_hooks(self):
        p = Plain()
        for _ in range(N):
            r = (isinstance(1, p), isinstance(2, p), issubclass(int, p), issubclass(str, p))
        self.assertEqual(r, (True, False, True, False))
        for _ in range(N):
            r = (isinstance(1, typing.Hashable), isinstance([], typing.Hashable),
                 issubclass(list, typing.Sequence), isinstance({}, typing.Mapping))
        self.assertEqual(r, (True, False, True, True))

    def test_descriptor_and_errors(self):
        s = Static()
        self.assertTrue(isinstance(2, s))
        self.assertFalse(isinstance(3, s))
        with self.assertRaises(KeyError):
            isinstance(5, Raises())

    def test_replaced_hook(self):
        class C:
            def __instancecheck__(self, obj):
                return True

        c = C()
        for _ in range(N // 10):
            self.assertTrue(isinstance(0, c))
        C.__instancecheck__ = lambda self, obj: False
        self.assertFalse(isinstance(0, c))
        del C.__instancecheck__
        with self.assertRaises(TypeError):
            isinstance(0, c)


if __name__ == "__main__":
    unittest.main()

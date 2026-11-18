"""`x in cls` for a class whose metaclass defines `__contains__`.

The metaclass's plain function is called with the class (`EnumType`'s
membership test, a user metaclass's), with no bound method made; its
result's truth is the answer, and its errors propagate.
"""

import enum
import unittest

N = 3000


class Color(enum.Enum):
    RED = 1
    GREEN = 2


class Perm(enum.Flag):
    R = 4
    W = 2


class Meta(type):
    def __contains__(cls, item):
        if item == "boom":
            raise ValueError("boom")
        return item in cls.items


class Bag(metaclass=Meta):
    items = (1, 2)


class SubMeta(Meta):
    pass


class Sub(metaclass=SubMeta):
    items = ("a",)


class Truthy:
    def __bool__(self):
        return True


class OddMeta(type):
    def __contains__(cls, item):
        return Truthy()


class Odd(metaclass=OddMeta):
    pass


class MetaclassContainsTests(unittest.TestCase):
    def test_enum(self):
        for _ in range(N):
            a = Color.RED in Color
            b = 1 in Color
            c = 3 in Color
            d = Perm.R | Perm.W in Perm
        self.assertEqual((a, b, c, d), (True, True, False, True))
        self.assertNotIn("x", Color)

    def test_user_metaclass(self):
        for _ in range(N):
            r = (1 in Bag, 3 in Bag, "a" in Sub, 2 not in Bag)
        self.assertEqual(r, (True, False, True, False))
        with self.assertRaises(ValueError):
            "boom" in Bag

    def test_result_truth(self):
        for _ in range(N):
            r = 0 in Odd
        self.assertIs(r, True)

    def test_plain_class_has_no_membership(self):
        class P:
            pass

        with self.assertRaises(TypeError):
            1 in P


if __name__ == "__main__":
    unittest.main()

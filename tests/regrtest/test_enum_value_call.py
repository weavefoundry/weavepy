"""`SomeEnum(value)` answered from the class's member maps.

With `enum`'s own `EnumType.__call__` and `Enum.__new__`, a member of the
class comes back as it is and a value that hashes natively is looked up
in `_value2member_map_`; misses, unhashable values, `_missing_` hooks,
other enums' members and overridden `__call__`s keep the Python path.
"""

import enum
import unittest

N = 3000


class Color(enum.Enum):
    RED = 1
    GREEN = 2
    ALIAS = 1


class Shade(enum.Enum):
    LIGHT = 1


class Perm(enum.IntFlag):
    R = 4
    W = 2


class Lenient(enum.Enum):
    A = "a"

    @classmethod
    def _missing_(cls, value):
        if isinstance(value, str) and value.lower() == "a":
            return cls.A
        return None


class Planet(enum.Enum):
    EARTH = (5.97, 6.37)

    def __init__(self, mass, radius):
        self.mass = mass


class Odd(enum.Enum):
    L = [1, 2]


class Meta(enum.EnumType):
    def __call__(cls, *args, **kw):
        return "overridden"


class Custom(enum.Enum, metaclass=Meta):
    X = 1


class EnumValueCallTests(unittest.TestCase):
    def test_values_and_members(self):
        for _ in range(N):
            a = Color(1)
            b = Color(Color.GREEN)
            c = Color(True)
            f = Perm(4)
        self.assertIs(a, Color.RED)
        self.assertIs(b, Color.GREEN)
        self.assertIs(c, Color.RED)
        self.assertIs(f, Perm.R)
        self.assertIs(Color(Color.ALIAS), Color.RED)

    def test_misses_and_hooks(self):
        for _ in range(100):
            with self.assertRaises(ValueError):
                Color(3)
            with self.assertRaises(ValueError):
                Color(Shade.LIGHT)
            self.assertIs(Lenient("A"), Lenient.A)
            self.assertEqual(Perm(6), Perm.R | Perm.W)
        self.assertIs(Planet((5.97, 6.37)), Planet.EARTH)
        self.assertIs(Odd([1, 2]), Odd.L)
        self.assertEqual(Custom(1), "overridden")

    def test_members_added_later(self):
        class Grow(enum.Enum):
            A = 1

        for _ in range(N):
            r = Grow(1)
        self.assertIs(r, Grow.A)
        with self.assertRaises(ValueError):
            Grow(2)


if __name__ == "__main__":
    unittest.main()

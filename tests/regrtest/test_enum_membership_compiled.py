"""x in SomeEnum and SomeEnum[name] from compiled loops: members, their values, aliases'
values, values of other types, an unhashable value, a member of another
enum, IntEnum and StrEnum values, a Flag class (its _missing_ answers),
and a metaclass overriding __contains__, each matching CPython."""

import enum
import unittest


class Color(enum.Enum):
    RED = 1
    GREEN = 2
    CRIMSON = 1
    LIST = [1, 2]


class Shape(enum.Enum):
    SQ = 1


class Num(enum.IntEnum):
    ONE = 1


class Txt(enum.StrEnum):
    A = "a"


class Perm(enum.Flag):
    R = 4
    W = 2


class OddMeta(enum.EnumType):
    def __contains__(cls, v):
        return v == "odd"


class Odd(enum.Enum, metaclass=OddMeta):
    X = 1


def test(n, x, cls):
    out = None
    for i in range(n):
        out = x in cls
    return out


class EnumMembershipTests(unittest.TestCase):
    def test_answers(self):
        cases = [
            (Color.RED, Color, True),
            (Color.CRIMSON, Color, True),
            (1, Color, True),
            (2, Color, True),
            (3, Color, False),
            ("RED", Color, False),
            ([1, 2], Color, True),
            ([3], Color, False),
            (Shape.SQ, Color, False),
            (1, Num, True),
            (Num.ONE, Num, True),
            (2.0, Num, False),
            ("a", Txt, True),
            ("b", Txt, False),
            (6, Perm, True),
            (Perm.R, Perm, True),
            (8, Perm, False),
            ("odd", Odd, True),
            (1, Odd, False),
        ]
        for n in (1, 300):
            for x, cls, want in cases:
                self.assertIs(test(n, x, cls), want, (x, cls))


class EnumItemTests(unittest.TestCase):
    def test_items(self):
        def get(n, cls, name):
            out = None
            for i in range(n):
                out = cls[name]
            return out

        for n in (1, 300):
            self.assertIs(get(n, Color, "RED"), Color.RED)
            self.assertIs(get(n, Color, "CRIMSON"), Color.RED)
            self.assertIs(get(n, Perm, "W"), Perm.W)
        with self.assertRaises(KeyError):
            get(300, Color, "BLUE")
        with self.assertRaises(KeyError):
            get(300, Color, 1)

        class M(enum.EnumType):
            def __getitem__(cls, name):
                return "meta"

        class E(enum.Enum, metaclass=M):
            A = 1

        self.assertEqual(get(300, E, "A"), "meta")


if __name__ == "__main__":
    unittest.main()

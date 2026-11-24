"""list(), tuple(), set() and sorted() of an enum class from compiled
loops: members in definition order without aliases, a Flag class's
canonical members, an empty enum, a metaclass that overrides __iter__,
and a member added after creation through the internal tables, each
matching CPython."""

import enum
import unittest


class Color(enum.Enum):
    RED = 1
    GREEN = 2
    CRIMSON = 1
    BLUE = 3


class Perm(enum.Flag):
    R = 4
    W = 2
    X = 1
    RW = 6


class Empty(enum.Enum):
    pass


class RevMeta(enum.EnumType):
    def __iter__(cls):
        return reversed(cls._member_names_)


class Rev(enum.Enum, metaclass=RevMeta):
    A = 1
    B = 2


def collect(n, cls):
    out = None
    for i in range(n):
        out = (list(cls), tuple(cls), len(set(cls)))
    return out


class EnumCollectTests(unittest.TestCase):
    def test_members(self):
        for n in (1, 300):
            self.assertEqual(collect(n, Color)[0], [Color.RED, Color.GREEN, Color.BLUE])
            self.assertEqual(collect(n, Color)[1], (Color.RED, Color.GREEN, Color.BLUE))
            self.assertEqual(collect(n, Perm)[0], [Perm.R, Perm.W, Perm.X])
            self.assertEqual(collect(n, Empty), ([], (), 0))
            self.assertEqual(collect(n, Rev)[0], ["B", "A"])
        self.assertEqual(sorted(Color, key=lambda m: m.value), [Color.RED, Color.GREEN, Color.BLUE])

    def test_extended(self):
        class E(enum.Enum):
            A = 1

        self.assertEqual(collect(300, E)[0], [E.A])
        member = object.__new__(E)
        member._name_ = "B"
        member._value_ = 2
        E._member_map_["B"] = member
        E._member_names_.append("B")
        self.assertEqual(collect(300, E)[0], [E.A, member])


if __name__ == "__main__":
    unittest.main()

"""for loops over enum classes and len() of them from compiled code: the
members in order without aliases, a member added to the tables while a
loop runs (seen, as the generator would see it), an empty enum, a
metaclass overriding __iter__ or __len__, a Flag class, and map over a
dict's __getitem__ with a missing key's KeyError, each matching
CPython."""

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


class Empty(enum.Enum):
    pass


class OddMeta(enum.EnumType):
    def __iter__(cls):
        return iter(["x", "y"])

    def __len__(cls):
        return 7


class Odd(enum.Enum, metaclass=OddMeta):
    A = 1


def loop(n, cls):
    out = None
    for i in range(n):
        got = []
        for m in cls:
            got.append(m)
        out = (got, len(cls))
    return out


class EnumLoopTests(unittest.TestCase):
    def test_loops(self):
        for n in (1, 300):
            self.assertEqual(loop(n, Color), ([Color.RED, Color.GREEN, Color.BLUE], 3))
            self.assertEqual(loop(n, Perm), ([Perm.R, Perm.W, Perm.X], 3))
            self.assertEqual(loop(n, Empty), ([], 0))
            self.assertEqual(loop(n, Odd), (["x", "y"], 7))

    def test_added_during_loop(self):
        def run(n):
            out = None
            for i in range(n):

                class E(enum.Enum):
                    A = 1

                got = []
                for m in E:
                    got.append(m.name if hasattr(m, "name") else m)
                    if len(got) == 1:
                        extra = object.__new__(E)
                        extra._name_ = "B"
                        extra._value_ = 2
                        E._member_map_["B"] = extra
                        E._member_names_.append("B")
                out = got
            return out

        self.assertEqual(run(300), ["A", "B"])

    def test_map_getitem(self):
        d = {"a": 1, "b": 2}

        def run(n, keys):
            out = None
            for i in range(n):
                out = list(map(d.__getitem__, keys))
            return out

        for n in (1, 300):
            self.assertEqual(run(n, ["b", "a"]), [2, 1])
        with self.assertRaises(KeyError):
            run(300, ["a", "z"])


if __name__ == "__main__":
    unittest.main()

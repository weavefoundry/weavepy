"""Reads of `__slots__` members in loops.

A member is read at its position in the class's slot layout once the
site has seen the class resolve the name to its slot descriptor. An
unset member, a deleted one, a subclass that replaces the member with a
property, a `__getattr__` fallback, and a lazily set member (pathlib's
cached parts) must behave as the ordinary lookup does.
"""

import pathlib
import unittest


class P:
    __slots__ = ("x", "y", "_cached")

    def __init__(self, x, y):
        self.x = x
        self.y = y

    @property
    def cached(self):
        try:
            return self._cached
        except AttributeError:
            self._cached = self.x * 10
            return self._cached


class Q(P):
    __slots__ = ()

    def __init__(self, y):
        self.y = y

    @property
    def x(self):
        return -1


class Fallback:
    __slots__ = ("a",)

    def __getattr__(self, name):
        return "fallback:" + name


def total_x(items):
    s = 0
    for p in items:
        s += p.x + p.y
    return s


class SlotMemberReadTest(unittest.TestCase):
    def test_reads(self):
        ps = [P(i, 2 * i) for i in range(1000)]
        self.assertEqual(total_x(ps), sum(3 * i for i in range(1000)))
        self.assertEqual(total_x([Q(6)] * 3), 3 * (-1 + 6))

    def test_lazy_member(self):
        ps = [P(i, 0) for i in range(500)]
        for _ in range(3):
            self.assertEqual(sum(p.cached for p in ps), 10 * sum(range(500)))

    def test_unset_and_deleted(self):
        p = P(1, 2)
        for _ in range(500):
            self.assertEqual(p.x, 1)
        del p.x
        with self.assertRaises(AttributeError):
            p.x
        f = Fallback()
        for _ in range(500):
            self.assertEqual(f.a, "fallback:a")
        f.a = 3
        self.assertEqual(f.a, 3)

    def test_pathlib(self):
        base = pathlib.PurePosixPath("/srv/app")
        for i in range(500):
            p = base / ("f%d.tar.gz" % i)
            self.assertEqual(p.name, "f%d.tar.gz" % i)
            self.assertEqual(p.suffixes, [".tar", ".gz"])
            self.assertEqual(p.parent, base)


if __name__ == "__main__":
    unittest.main()

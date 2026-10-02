"""Compiled code that constructs a class keeps checking how it constructs.

A loop builds instances of a global class whose construction tier 2 burns
in, and an interpreted hook changes the class or its `__init__` partway
through: the constructions after the change must see it.
"""

import unittest


class Plain:
    def __init__(self, v):
        self.v = v


class Rebound:
    def __init__(self, v):
        self.v = v


class Swapped:
    def __init__(self, v):
        self.v = v


class GainsNew:
    def __init__(self, v):
        self.v = v


def build_plain(n, hook):
    t = 0
    for i in range(n):
        t += Plain(i).v
        hook(i)
    return t


def build_rebound(n, hook):
    t = 0
    for i in range(n):
        t += Rebound(i).v
        hook(i)
    return t


def build_swapped(n, hook):
    t = 0
    for i in range(n):
        t += Swapped(i).v
        hook(i)
    return t


def build_gains_new(n, hook):
    t = 0
    for i in range(n):
        t += GainsNew(i).v
        hook(i)
    return t


def noop(i):
    pass


def triangle(n):
    # The sum of 0, 1, ..., n - 1.
    return n * (n - 1) // 2


class CtorGuardTests(unittest.TestCase):
    N = 3000

    def test_plain(self):
        self.assertEqual(build_plain(self.N, noop), triangle(self.N))
        self.assertEqual(build_plain(self.N, noop), triangle(self.N))

    def test_init_rebound_midway(self):
        def init(self, v):
            self.v = -1

        half = self.N // 2

        def hook(i):
            if i == half:
                Rebound.__init__ = init

        self.assertEqual(build_rebound(self.N, noop), triangle(self.N))
        expected = triangle(half + 1) - (self.N - half - 1)
        saved = Rebound.__init__
        try:
            self.assertEqual(build_rebound(self.N, hook), expected)
        finally:
            Rebound.__init__ = saved

    def test_init_code_swapped_midway(self):
        def other(self, v):
            self.v = 2 * v

        half = self.N // 2

        def hook(i):
            if i == half:
                Swapped.__init__.__code__ = other.__code__

        self.assertEqual(build_swapped(self.N, noop), triangle(self.N))
        expected = triangle(half + 1) + 2 * (triangle(self.N) - triangle(half + 1))
        saved = Swapped.__init__.__code__
        try:
            self.assertEqual(build_swapped(self.N, hook), expected)
        finally:
            Swapped.__init__.__code__ = saved

    def test_new_added_midway(self):
        half = self.N // 2

        def new(cls, v):
            o = object.__new__(cls)
            o.extra = True
            return o

        def hook(i):
            if i == half:
                GainsNew.__new__ = new

        self.assertEqual(build_gains_new(self.N, noop), triangle(self.N))
        self.assertEqual(build_gains_new(self.N, hook), triangle(self.N))
        self.assertTrue(GainsNew(1).extra)


if __name__ == "__main__":
    unittest.main()

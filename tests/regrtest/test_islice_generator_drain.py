"""`list(islice(gen, ...))` steps the generator in a row; the items, how
far the generator advances, the slice's state after, and raises match
CPython."""

import itertools
import unittest


def gen(n):
    for i in range(n):
        yield i


class Boom(Exception):
    pass


def raising():
    yield 1
    yield 2
    raise Boom


def returning():
    yield 1
    return "v"


class IsliceGeneratorDrain(unittest.TestCase):
    def test_bounds(self):
        cases = [(5,), (2, 8), (1, 9, 3), (0, None, 2), (3, 3), (10, 20), (None,)]
        for args in cases:
            for _ in range(50):
                self.assertEqual(list(itertools.islice(gen(12), *args)),
                                 list(itertools.islice(range(12), *args)))

    def test_generator_position(self):
        g = gen(10)
        self.assertEqual(list(itertools.islice(g, 2, 5)), [2, 3, 4])
        self.assertEqual(next(g), 5)
        g = gen(10)
        self.assertEqual(list(itertools.islice(g, 3, 3)), [])
        self.assertEqual(next(g), 3)
        g = gen(10)
        s = itertools.islice(g, 0, 6, 2)
        self.assertEqual(next(s), 0)
        self.assertEqual(list(s), [2, 4])
        self.assertEqual(list(s), [])
        # (The item before the stop bound was taken as a skipped one.)
        self.assertEqual(next(g), 6)

    def test_raise_and_return(self):
        s = itertools.islice(raising(), 5)
        with self.assertRaises(Boom):
            list(s)
        self.assertEqual(list(s), [])
        self.assertEqual(list(itertools.islice(returning(), 5)), [1])


if __name__ == "__main__":
    unittest.main()

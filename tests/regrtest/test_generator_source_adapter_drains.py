"""list() of zip, map, filter and enumerate adapters with a generator
among their sources (each step taken by the general path, the pure one
skipped): results, a generator first or second in a zip and how far each
source is consumed, a map with a generator and a list, a filter whose
generator raises partway, and sources that mix generators with plain
cursors, each matching CPython."""

import unittest


def gen(n, fail_at=None):
    for i in range(n):
        if i == fail_at:
            raise ValueError(i)
        yield i


class AdapterGeneratorTests(unittest.TestCase):
    def test_results(self):
        for k in range(300):
            z = list(zip([1, 2, 3], gen(5)))
            m = list(map(lambda a, b: a + b, gen(3), [10, 20, 30, 40]))
            f = list(filter(None, gen(4)))
            e = list(enumerate(gen(2), 1))
        self.assertEqual(z, [(1, 0), (2, 1), (3, 2)])
        self.assertEqual(m, [10, 21, 32])
        self.assertEqual(f, [1, 2, 3])
        self.assertEqual(e, [(1, 0), (2, 1)])

    def test_consumption(self):
        g = gen(5)
        self.assertEqual(list(zip([1, 2], g)), [(1, 0), (2, 1)])
        self.assertEqual(list(g), [2, 3, 4])
        g = gen(5)
        it = iter([1, 2])
        self.assertEqual(list(zip(g, it)), [(0, 1), (1, 2)])
        self.assertEqual(list(g), [3, 4])
        g = gen(2)
        xs = iter([1, 2, 3, 4])
        self.assertEqual(list(map(max, g, xs)), [1, 2])
        self.assertEqual(list(xs), [3, 4])

    def test_raise_partway(self):
        out = []
        with self.assertRaises(ValueError):
            for x in filter(None, gen(5, fail_at=3)):
                out.append(x)
        self.assertEqual(out, [1, 2])
        with self.assertRaises(ValueError):
            list(zip(gen(5, fail_at=2), [1, 2, 3]))


if __name__ == "__main__":
    unittest.main()

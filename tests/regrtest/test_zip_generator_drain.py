"""`list(zip(...))` with generator sources steps each row's sources in
order; rows, what each source has given up at the end, aliased sources,
and raises match CPython."""

import unittest


def gen(n, start=0):
    for i in range(start, start + n):
        yield i


class Boom(Exception):
    pass


def raising(n):
    yield from range(n)
    raise Boom


class ZipGeneratorDrain(unittest.TestCase):
    def test_rows(self):
        for _ in range(200):
            self.assertEqual(list(zip(gen(3), gen(5, 10))), [(0, 10), (1, 11), (2, 12)])
            self.assertEqual(list(zip(gen(4), "abc", [7, 8, 9, 10])),
                             [(0, "a", 7), (1, "b", 8), (2, "c", 9)])
            self.assertEqual(list(zip(gen(0), gen(3))), [])

    def test_consumption_at_end(self):
        a, b = gen(5), gen(2, 10)
        self.assertEqual(list(zip(a, b)), [(0, 10), (1, 11)])
        # (The first source's third item was taken and dropped.)
        self.assertEqual(list(a), [3, 4])
        a, b = gen(2), gen(5, 10)
        self.assertEqual(list(zip(a, b)), [(0, 10), (1, 11)])
        self.assertEqual(list(b), [12, 13, 14])

    def test_aliased_and_raising(self):
        g = gen(6)
        self.assertEqual(list(zip(g, g)), [(0, 1), (2, 3), (4, 5)])
        with self.assertRaises(Boom):
            list(zip(raising(3), gen(10)))
        z = zip(gen(3), raising(1))
        with self.assertRaises(Boom):
            list(z)


if __name__ == "__main__":
    unittest.main()

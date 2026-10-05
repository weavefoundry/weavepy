"""Compiled code reads, writes, appends to and iterates scalar lists in line.

Each loop runs long enough for the tier-2 compiler to take it; the lists
then change shape under the compiled code, and every change must reach
the generic path with the interpreter's result.
"""

import unittest


def total(xs, n):
    t = 0
    for i in range(n):
        t += xs[i % len(xs)] + xs[-1]
    return t


def scale(xs, n, k):
    for i in range(n):
        j = i % len(xs)
        xs[j] = xs[j] * k
    return xs


def fill(n):
    out = []
    for i in range(n):
        out.append(i * 2)
    return out


def fill_floats(n):
    out = []
    for i in range(n):
        out.append(i * 0.5)
    return out


def walk(xs):
    t = 0
    for x in xs:
        t += x
    return t


class InlineListTests(unittest.TestCase):
    N = 3000

    def test_reads(self):
        xs = list(range(10))
        self.assertEqual(total(xs, self.N), sum(xs[i % 10] + 9 for i in range(self.N)))

    def test_out_of_range(self):
        xs = [1, 2, 3]

        def get(xs, n, idx):
            t = 0
            for _ in range(n):
                t += xs[idx]
            return t

        self.assertEqual(get(xs, self.N, -3), self.N)
        with self.assertRaises(IndexError):
            get(xs, 1, 3)
        with self.assertRaises(IndexError):
            get(xs, 1, -4)

    def test_element_type_changes(self):
        xs = list(range(10))
        self.assertEqual(total(xs, self.N), total(list(range(10)), self.N))
        xs[3] = 2.5
        self.assertEqual(total(xs, 10), sum(xs) + 9 * 10)
        xs[3] = "a"
        with self.assertRaises(TypeError):
            total(xs, 10)

    def test_writes(self):
        xs = [1.0] * 8
        scale(xs, self.N, 1.0001)
        ref = [1.0] * 8
        for i in range(self.N):
            ref[i % 8] = ref[i % 8] * 1.0001
        self.assertEqual(xs, ref)

    def test_write_over_heap_element(self):
        xs = [1.0] * 4
        scale(xs, self.N, 1.0)
        marker = [1.0]
        xs[1] = marker
        with self.assertRaises(TypeError):
            scale(xs, 4, 2.0)

    def test_appends_grow(self):
        self.assertEqual(fill(self.N), [i * 2 for i in range(self.N)])
        self.assertEqual(fill_floats(self.N), [i * 0.5 for i in range(self.N)])
        self.assertEqual(fill(5), [0, 2, 4, 6, 8])

    def test_iteration(self):
        xs = list(range(self.N))
        self.assertEqual(walk(xs), sum(xs))
        self.assertEqual(walk([]), 0)
        ys = [1, 2, 3.5]
        self.assertEqual(walk(ys), 6.5)
        with self.assertRaises(TypeError):
            walk([1, "x"])

    def test_iteration_sees_growth(self):
        def grow(xs, limit):
            n = 0
            for x in xs:
                n += 1
                if len(xs) < limit:
                    xs.append(x + 1)
            return n

        xs = [0]
        self.assertEqual(grow(xs, self.N), self.N)
        self.assertEqual(xs[-1], self.N - 1)

    def test_bools(self):
        flags = [True, False] * 100
        self.assertEqual(walk(flags * 10), 1000)


if __name__ == "__main__":
    unittest.main()

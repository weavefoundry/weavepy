"""sorted() and list.sort() with key=functools.cmp_to_key(mycmp) compare
the items through mycmp directly: stable order, reverse, a mycmp result
that is not an int (its own < 0 decides), an error from mycmp, a mycmp
that mutates the list being sorted, and a key class that stays immutable,
each matching CPython."""

import functools
import unittest


def three_way(a, b):
    return (a > b) - (a < b)


class Res:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v < other


class CmpToKeySortTests(unittest.TestCase):
    def test_order_and_stability(self):
        xs = [(1, "a"), (0, "b"), (1, "c"), (0, "d")]
        k = functools.cmp_to_key(lambda a, b: a[0] - b[0])
        for _ in range(300):
            self.assertEqual(sorted(xs, key=k), [(0, "b"), (0, "d"), (1, "a"), (1, "c")])
            self.assertEqual(
                sorted(xs, key=k, reverse=True), [(1, "a"), (1, "c"), (0, "b"), (0, "d")]
            )
            ys = list(range(20, 0, -1))
            ys.sort(key=functools.cmp_to_key(three_way))
            self.assertEqual(ys, list(range(1, 21)))

    def test_non_int_result(self):
        k = functools.cmp_to_key(lambda a, b: Res(a - b))
        self.assertEqual(sorted([3, 1, 2], key=k), [1, 2, 3])
        k = functools.cmp_to_key(lambda a, b: float(a - b))
        self.assertEqual(sorted([3, 1, 2], key=k), [1, 2, 3])

    def test_error(self):
        calls = []

        def bad(a, b):
            calls.append((a, b))
            raise ValueError("no")

        with self.assertRaises(ValueError):
            sorted([1, 2, 3], key=functools.cmp_to_key(bad))
        self.assertEqual(len(calls), 1)
        self.assertEqual(sorted([5], key=functools.cmp_to_key(bad)), [5])

    def test_mutation(self):
        L = [3, 2, 1]

        def mut(a, b):
            L.append(0)
            return a - b

        with self.assertRaises(ValueError):
            L.sort(key=functools.cmp_to_key(mut))

    def test_wrappers(self):
        k = functools.cmp_to_key(three_way)
        self.assertTrue(k(1) < k(2))
        self.assertTrue(k(2) >= k(2))
        with self.assertRaises(TypeError):
            type(k).__call__ = None
        with self.assertRaises(TypeError):
            type(k(1)).__lt__ = None
        self.assertEqual(sorted([2, 1], key=k), [1, 2])


if __name__ == "__main__":
    unittest.main()

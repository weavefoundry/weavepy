"""`list()` of `itertools`' combinatoric iterators.

`list(product(...))`, and the same of `permutations`, `combinations`,
`combinations_with_replacement` and a bounded `repeat`, step the iterator
to its end at once. The rows match a one-at-a-time walk, a partly consumed
iterator gives the rest, and an exhausted one gives nothing more.
"""

import itertools
import unittest


def product_rows(pools, repeat=1):
    pools = [tuple(p) for p in pools] * repeat
    rows = [()]
    for pool in pools:
        rows = [r + (x,) for r in rows for x in pool]
    return rows


class RowsCollectedTests(unittest.TestCase):
    def test_product(self):
        for _ in range(50):
            self.assertEqual(list(itertools.product(range(3), "ab")), product_rows([range(3), "ab"]))
            self.assertEqual(list(itertools.product(range(4), repeat=3)), product_rows([range(4)], 3))
            self.assertEqual(list(itertools.product()), [()])
            self.assertEqual(list(itertools.product([], [1])), [])
            self.assertEqual(list(itertools.product([1], repeat=0)), [()])

    def test_others(self):
        for _ in range(50):
            self.assertEqual(len(list(itertools.permutations(range(5), 2))), 20)
            self.assertEqual(list(itertools.permutations("abc")), [tuple(p) for p in ["abc", "acb", "bac", "bca", "cab", "cba"]])
            self.assertEqual(list(itertools.combinations(range(5), 3))[:3], [(0, 1, 2), (0, 1, 3), (0, 1, 4)])
            self.assertEqual(list(itertools.combinations(range(2), 3)), [])
            self.assertEqual(list(itertools.combinations_with_replacement("ab", 2)), [("a", "a"), ("a", "b"), ("b", "b")])
            self.assertEqual(list(itertools.repeat("x", 3)), ["x", "x", "x"])
            self.assertEqual(list(itertools.repeat("x", 0)), [])
            self.assertEqual(list(itertools.repeat("x", -2)), [])
            self.assertEqual(list(itertools.permutations(range(3), 0)), [()])

    def test_partly_consumed_and_exhausted(self):
        it = itertools.product("ab", repeat=2)
        self.assertEqual(next(it), ("a", "a"))
        self.assertEqual(list(it), [("a", "b"), ("b", "a"), ("b", "b")])
        self.assertEqual(list(it), [])
        self.assertIsNone(next(it, None))
        c = itertools.combinations(range(4), 2)
        next(c)
        next(c)
        self.assertEqual(list(c), [(0, 3), (1, 2), (1, 3), (2, 3)])
        r = itertools.repeat(7, 4)
        next(r)
        self.assertEqual(list(r), [7, 7, 7])
        self.assertEqual(r.__length_hint__(), 0)

    def test_items_shared_not_copied(self):
        obj = [1]
        rows = list(itertools.product([obj], repeat=2))
        self.assertIs(rows[0][0], obj)
        self.assertIs(rows[0][1], obj)
        rows = list(itertools.repeat(obj, 2))
        self.assertIs(rows[1], obj)
        self.assertEqual(tuple(itertools.product("ab")), (("a",), ("b",)))
        self.assertEqual(sorted(itertools.permutations([2, 1])), [(1, 2), (2, 1)])


if __name__ == "__main__":
    unittest.main()

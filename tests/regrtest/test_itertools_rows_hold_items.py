"""The rows of `itertools`' combinatoric iterators hold their items.

Rows of `product`, `permutations`, `combinations` and
`combinations_with_replacement` over pools of heap objects (strings built
at run time, lists, instances) match the itertools documentation's recipes,
whether collected at once or taken one at a time. Each row holds a
reference to each item it shows: the items' reference counts rise by the
rows' share and fall back once the rows go, and the items are freed when
nothing else holds them.
"""

import gc
import itertools
import sys
import unittest
import weakref


class Item:
    def __init__(self, n):
        self.n = n

    def __eq__(self, other):
        return isinstance(other, Item) and other.n == self.n

    def __hash__(self):
        return self.n


def product_rows(*pools, repeat=1):
    pools = [tuple(p) for p in pools] * repeat
    rows = [()]
    for pool in pools:
        rows = [r + (x,) for r in rows for x in pool]
    return rows


def permutation_rows(pool, r=None):
    pool = tuple(pool)
    n = len(pool)
    r = n if r is None else r
    return [tuple(pool[i] for i in ix) for ix in product_rows(range(n), repeat=r)
            if len(set(ix)) == r]


def combination_rows(pool, r):
    pool = tuple(pool)
    return [tuple(pool[i] for i in ix) for ix in permutation_rows(range(len(pool)), r)
            if sorted(ix) == list(ix)]


def cwr_rows(pool, r):
    pool = tuple(pool)
    return [tuple(pool[i] for i in ix) for ix in product_rows(range(len(pool)), repeat=r)
            if sorted(ix) == list(ix)]


def pools():
    return [
        ["s%d" % i * 3 for i in range(5)],
        [[i] for i in range(4)],
        [Item(i) for i in range(6)],
    ]


class RowsHoldItemsTests(unittest.TestCase):
    def test_against_recipes(self):
        for _ in range(40):
            for pool in pools():
                self.assertEqual(list(itertools.combinations(pool, 3)), combination_rows(pool, 3))
                self.assertEqual(list(itertools.combinations_with_replacement(pool, 2)),
                                 cwr_rows(pool, 2))
                self.assertEqual(list(itertools.permutations(pool, 2)), permutation_rows(pool, 2))
                self.assertEqual(list(itertools.product(pool, pool[:2])),
                                 product_rows(pool, pool[:2]))
                stepped = []
                it = itertools.combinations(pool, 2)
                for row in it:
                    stepped.append(row)
                self.assertEqual(stepped, combination_rows(pool, 2))

    def test_counts(self):
        for make in (lambda p: itertools.combinations(p, 2),
                     lambda p: itertools.combinations_with_replacement(p, 2),
                     lambda p: itertools.permutations(p, 2),
                     lambda p: itertools.product(p, repeat=2)):
            for _ in range(30):
                pool = [Item(i) for i in range(4)]
                before = [sys.getrefcount(x) for x in pool]
                rows = list(make(pool))
                shown = [sum(row.count(x) for row in rows) for x in pool]
                after = [sys.getrefcount(x) for x in pool]
                self.assertEqual([a - b for a, b in zip(after, before)], shown)
                del rows
                self.assertEqual([sys.getrefcount(x) for x in pool], before)

    def test_items_freed(self):
        for _ in range(30):
            pool = [Item(i) for i in range(5)]
            refs = [weakref.ref(x) for x in pool]
            rows = list(itertools.combinations(pool, 3))
            rows += list(itertools.product(pool, repeat=2))
            first = rows[0][0]
            del pool
            gc.collect()
            self.assertTrue(all(r() is not None for r in refs))
            del rows
            gc.collect()
            self.assertIs(refs[0](), first)
            del first
            gc.collect()
            self.assertTrue(all(r() is None for r in refs))


if __name__ == "__main__":
    unittest.main()

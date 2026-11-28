"""`zip` over many sources, and over the same source several times.

`zip(*rows)` transposes a table: it has one source per row, and each step
takes one item from each. The cost of a step grows with the number of
sources, not with its square. The grouping idiom `zip(*[iter(xs)] * n)`
passes the *same* iterator n times, and each step then takes n successive
items from it: the two shapes must not be confused, however many sources
there are.
"""

import itertools
import time
import unittest


class TransposeTests(unittest.TestCase):
    def test_transpose_of_rows(self):
        for width in (1, 2, 3, 9, 40):
            for height in (0, 1, 2, 8, 9, 100, 3000):
                rows = [tuple(r * 1000 + c for c in range(width)) for r in range(height)]
                with self.subTest(width=width, height=height):
                    columns = list(zip(*rows))
                    if height == 0:
                        self.assertEqual(columns, [])
                        continue
                    self.assertEqual(len(columns), width)
                    for c, column in enumerate(columns):
                        self.assertEqual(len(column), height)
                        self.assertEqual(column, tuple(r * 1000 + c for r in range(height)))
                    self.assertEqual([tuple(r) for r in zip(*columns)], rows)

    def test_unzip_of_pairs(self):
        pairs = [(i, str(i)) for i in range(20000)]
        numbers, names = zip(*pairs)
        self.assertEqual(numbers, tuple(range(20000)))
        self.assertEqual(names[:3], ("0", "1", "2"))
        self.assertEqual(len(names), 20000)
        self.assertIs(names[777], pairs[777][1])

    def test_ragged_rows_stop_at_the_shortest(self):
        rows = [list(range(5))] * 50 + [list(range(3))] + [list(range(5))] * 50
        self.assertEqual(len(list(zip(*rows))), 3)
        with self.assertRaises(ValueError):
            list(zip(*rows, strict=True))
        self.assertEqual(len(list(zip(*([list(range(4))] * 101), strict=True))), 4)

    def test_sources_of_mixed_kinds(self):
        sources = [
            [1, 2, 3], (4, 5, 6), "abc", range(7, 10), iter([10, 11, 12]), {13: 0, 14: 0, 15: 0},
            (x for x in (16, 17, 18)), b"xyz", itertools.count(19), [22, 23, 24], reversed([27, 26, 25]),
        ]
        got = list(zip(*sources))
        self.assertEqual(
            got,
            [
                (1, 4, "a", 7, 10, 13, 16, 120, 19, 22, 25),
                (2, 5, "b", 8, 11, 14, 17, 121, 20, 23, 26),
                (3, 6, "c", 9, 12, 15, 18, 122, 21, 24, 27),
            ],
        )

    def test_steps_taken_one_at_a_time(self):
        rows = [[r, r + 1, r + 2] for r in range(500)]
        z = zip(*rows)
        first = next(z)
        rows[0][1] = "changed"
        second = next(z)
        self.assertEqual(first[:3], (0, 1, 2))
        self.assertEqual(second[0], "changed")
        self.assertEqual(second[1], 2)
        third = next(z)
        self.assertEqual(third[499], 501)
        with self.assertRaises(StopIteration):
            next(z)
        self.assertIsNot(first, second)
        self.assertEqual(first[499], 499)


class SharedSourceTests(unittest.TestCase):
    def test_grouping_with_one_iterator(self):
        for n in (1, 2, 3, 8, 9, 10, 50):
            for total in (0, 1, n, n * 7, n * 7 + n - 1):
                with self.subTest(n=n, total=total):
                    it = iter(range(total))
                    groups = list(zip(*[it] * n))
                    want = [tuple(range(g * n, g * n + n)) for g in range(total // n)]
                    self.assertEqual(groups, want)

    def test_grouping_other_iterators(self):
        text = "abcdefghij"
        self.assertEqual(list(zip(*[iter(text)] * 3)), [("a", "b", "c"), ("d", "e", "f"), ("g", "h", "i")])
        gen = (x * x for x in range(30))
        self.assertEqual(list(zip(*[gen] * 10))[2], tuple(x * x for x in range(20, 30)))
        self.assertEqual(
            list(zip(*[iter(range(24))] * 12)),
            [tuple(range(12)), tuple(range(12, 24))],
        )
        chained = itertools.chain([1, 2], [3, 4], [5, 6])
        self.assertEqual(list(zip(*[chained] * 2)), [(1, 2), (3, 4), (5, 6)])

    def test_a_shared_iterator_among_distinct_ones(self):
        it = iter(range(100))
        others = [list(range(1000, 1010)) for _ in range(20)]
        got = list(zip(it, *others, it))
        self.assertEqual(len(got), 10)
        self.assertEqual(got[0][0], 0)
        self.assertEqual(got[0][-1], 1)
        self.assertEqual(got[9][0], 18)
        self.assertEqual(got[9][-1], 19)
        self.assertEqual(got[3][1:-1], (1003,) * 20)
        # (The step that found a source finished had already taken one.)
        self.assertEqual(next(it), 21)

    def test_the_same_list_many_times_is_not_shared(self):
        xs = [1, 2, 3]
        self.assertEqual(list(zip(*[xs] * 12)), [(1,) * 12, (2,) * 12, (3,) * 12])


class CostTests(unittest.TestCase):
    @staticmethod
    def best(fn, repeat=5):
        best = float("inf")
        for _ in range(repeat):
            start = time.perf_counter()
            fn()
            best = min(best, time.perf_counter() - start)
        return best

    def test_a_step_costs_its_sources_once(self):
        def transpose(height):
            rows = [(r, r + 1, r + 2) for r in range(height)]
            return lambda: list(zip(*rows))

        small = self.best(transpose(1_000))
        large = self.best(transpose(16_000))
        # (Sixteen times the rows: sixteen times the work. Comparing every
        # pair of sources at every step made it 256 times.)
        self.assertLess(large, small * 60 + 0.01)


if __name__ == "__main__":
    unittest.main()

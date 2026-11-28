"""`for i, (x, y) in enumerate(adapter)` steps the index and the adapter's
item without building the outer pair; the results, the counter, and the
adapters' end match the general step."""

import itertools
import unittest


class EnumerateAdapterPairs(unittest.TestCase):
    def test_enumerate_zip_unpacked(self):
        a = list(range(50))
        b = [str(x) for x in range(100, 150)]
        out = []
        for _ in range(200):
            out = []
            for i, (x, y) in enumerate(zip(a, b)):
                out.append((i, x, y))
        self.assertEqual(out, [(i, i, str(100 + i)) for i in range(50)])

    def test_start_and_shorter_source(self):
        for _ in range(200):
            got = [(i, x, y) for i, (x, y) in enumerate(zip("abc", [1, 2]), 10)]
        self.assertEqual(got, [(10, "a", 1), (11, "b", 2)])

    def test_counter_survives_break(self):
        e = enumerate(zip(range(10), range(10, 20)))
        for _ in range(3):
            for i, (x, y) in e:
                if i % 3 == 2:
                    break
        self.assertEqual(next(e), (9, (9, 19)))
        self.assertRaises(StopIteration, next, e)

    def test_items_kept_by_consumer(self):
        kept = []
        for _ in range(100):
            kept = []
            for i, pair in enumerate(zip([1, 2, 3], "xyz")):
                kept.append(pair)
        self.assertEqual(kept, [(1, "x"), (2, "y"), (3, "z")])
        self.assertEqual(len({id(p) for p in kept}), 3)

    def test_other_adapters(self):
        for _ in range(100):
            got = [(i, v) for i, v in enumerate(map(abs, [-1, -2, 3]))]
            tw = [(i, v) for i, v in enumerate(itertools.takewhile(lambda v: v < 3, [1, 2, 3, 1]))]
            fl = [(i, v) for i, v in enumerate(filter(None, [0, 5, 0, 6]))]
        self.assertEqual(got, [(0, 1), (1, 2), (2, 3)])
        self.assertEqual(tw, [(0, 1), (1, 2)])
        self.assertEqual(fl, [(0, 5), (1, 6)])

    def test_nested_unpack_of_three(self):
        for _ in range(100):
            got = [(i, a + b + c) for i, (a, b, c) in enumerate(zip([1, 2], [3, 4], [5, 6]))]
        self.assertEqual(got, [(0, 9), (1, 12)])

    def test_strict_zip_end(self):
        def run():
            for i, (x, y) in enumerate(zip([1, 2, 3], [4, 5], strict=True)):
                pass
        for _ in range(50):
            self.assertRaises(ValueError, run)


if __name__ == "__main__":
    unittest.main()

"""Subscripts in frameless leaf evaluation.

Small functions such as `key=lambda t: t[1]` are evaluated without an
activation when native code calls them. A subscript there must give the
same result as the full call, and anything it can't settle (a miss, an
unusual container) must fall back so the full call raises or dispatches
exactly as usual.
"""

import unittest


class LeafSubscripts(unittest.TestCase):
    def test_sort_keys(self):
        pairs = [(3, "c"), (1, "a"), (2, "b")] * 50
        self.assertEqual(sorted(pairs, key=lambda t: t[1])[:3], [(1, "a")] * 3)
        self.assertEqual(sorted(pairs, key=lambda t: t[-2])[-1], (3, "c"))
        rows = [[i, -i] for i in range(100)]
        self.assertEqual(max(rows, key=lambda r: r[1]), [0, 0])
        words = ["delta", "alpha", "charlie", "bravo"] * 30
        self.assertEqual(sorted(words, key=lambda w: w[1])[0], "delta")
        self.assertEqual(min(words, key=lambda w: w[-1]), "delta")

    def test_non_ascii_strings(self):
        words = ["żółw", "ąb", "éa", "zz"] * 30
        self.assertEqual(sorted(words, key=lambda w: w[1])[0], "éa")

    def test_dicts(self):
        tables = [{"k": i, 7: -i} for i in range(60)]
        self.assertEqual(max(tables, key=lambda d: d["k"])["k"], 59)
        self.assertEqual(max(tables, key=lambda d: d[7])["k"], 0)

    def test_misses_raise(self):
        items = [(1,), (2,)] * 40
        with self.assertRaises(IndexError):
            sorted(items, key=lambda t: t[1])
        dicts = [{"a": 1}] * 40
        with self.assertRaises(KeyError):
            sorted(dicts, key=lambda d: d["b"])
        strs = ["a", "bb"] * 40
        with self.assertRaises(IndexError):
            sorted(strs, key=lambda s: s[1])

    def test_subclasses_dispatch(self):
        class D(dict):
            def __missing__(self, key):
                return 42

        class L(list):
            def __getitem__(self, i):
                return -super().__getitem__(i)

        ds = [D(a=i) for i in range(40)]
        self.assertEqual(sorted(ds, key=lambda d: d["zz"])[0]["a"], 0)
        ls = [L([i]) for i in range(40)]
        self.assertEqual(max(ls, key=lambda x: x[0]), [0])

    def test_warm_loop(self):
        def second(t):
            return t[1]

        data = [(i, i * i) for i in range(30)]
        total = 0
        for _ in range(3000):
            total += max(map(second, data))
        self.assertEqual(total, 3000 * 29 * 29)


if __name__ == "__main__":
    unittest.main()

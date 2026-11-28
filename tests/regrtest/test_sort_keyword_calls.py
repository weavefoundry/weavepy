"""`list.sort` and `sorted` called with keywords.

`xs.sort(reverse=True)`, `xs.sort(key=None)` and `sorted(xs, key=None,
reverse=...)` of plain numbers or strings sort in place of the full call,
with CPython's stability: a reverse sort keeps equal items in their
original order. Any other key, item or argument shape takes the full call
and its errors, including a positional argument to `list.sort` and
`sorted`'s positional-only iterable passed by keyword.
"""

import unittest


class SortKeywordTests(unittest.TestCase):
    def test_reverse_and_none_key(self):
        for _ in range(300):
            xs = [3, 1, 2]
            xs.sort(reverse=True)
            self.assertEqual(xs, [3, 2, 1])
            xs.sort(key=None)
            self.assertEqual(xs, [1, 2, 3])
            xs.sort(key=None, reverse=1)
            self.assertEqual(xs, [3, 2, 1])
            xs.sort(reverse=False)
            self.assertEqual(xs, [1, 2, 3])
            ws = ["b", "a", "é", "c"]
            ws.sort(reverse=True)
            self.assertEqual(ws, ["é", "c", "b", "a"])
            self.assertEqual(sorted((2.5, 1, True), reverse=True), [2.5, 1, True])
            self.assertEqual(sorted([3, 1, 2], key=None), [1, 2, 3])
            self.assertEqual(sorted([], reverse=True), [])

    def test_stability(self):
        for _ in range(200):
            xs = [1, True, 1.0, 0, False]
            xs.sort(reverse=True)
            self.assertEqual([type(x) for x in xs], [int, bool, float, int, bool])
            ys = sorted([1.0, 1, True, 0.0, 0], reverse=True)
            self.assertEqual([type(y) for y in ys], [float, int, bool, float, int])
            src = (True, 1, 1.0)
            out = sorted(src, key=None)
            self.assertEqual([type(v) for v in out], [bool, int, float])

    def test_other_shapes(self):
        xs = ["bb", "a", "ccc"]
        for _ in range(200):
            xs.sort(key=len)
            self.assertEqual(xs, ["a", "bb", "ccc"])
            xs.sort(key=len, reverse=True)
            self.assertEqual(xs, ["ccc", "bb", "a"])
            self.assertEqual(sorted(iter([2, 1]), reverse=True), [2, 1])
            self.assertEqual(sorted({3, 1}, reverse=True), [3, 1])
        with self.assertRaises(TypeError):
            [1, "a"].sort(reverse=True)
        with self.assertRaises(TypeError):
            sorted([1, "a"], reverse=True)
        with self.assertRaises(TypeError):
            [1].sort(None)
        with self.assertRaises(TypeError):
            [1].sort(None, reverse=True)
        with self.assertRaises(TypeError):
            [1].sort(nope=True)
        with self.assertRaises(TypeError):
            sorted(iterable=[1])
        with self.assertRaises(TypeError):
            sorted([1], nope=1)
        ys = [2, 1]
        ys.sort(reverse="yes")
        self.assertEqual(ys, [2, 1])

        class L(list):
            pass

        z = L([2, 3, 1])
        z.sort(reverse=True)
        self.assertEqual(z, [3, 2, 1])

    def test_tuples(self):
        for _ in range(200):
            ps = [(2, "b"), (1, "z"), (2, "a"), (1, "a", 0), (1, "a")]
            ps.sort()
            self.assertEqual(ps, [(1, "a"), (1, "a", 0), (1, "z"), (2, "a"), (2, "b")])
            ps.sort(reverse=True)
            self.assertEqual(ps, [(2, "b"), (2, "a"), (1, "z"), (1, "a", 0), (1, "a")])
            self.assertEqual(sorted([(1, True), (1, 0), ()]), [(), (1, 0), (1, True)])
            same = [(1, True), (1, 1)]
            same.sort(reverse=True)
            self.assertEqual([type(t[1]) for t in same], [bool, int])
        with self.assertRaises(TypeError) as cm:
            [(1, "a"), (1, 2)].sort()
        self.assertIn("'<' not supported", str(cm.exception))
        nested = [(1, (2,)), (1, (1,))]
        nested.sort()
        self.assertEqual(nested, [(1, (1,)), (1, (2,))])

    def test_native_keys(self):
        import collections
        import operator

        words = ["ccc", "a", "bb", "dd", "e"]
        items = [("x", 3), ("y", 1), ("z", 2), ("w", 1)]

        class P:
            def __init__(self, n):
                self.n = n

        ps = [P(3), P(1), P(2)]
        for _ in range(200):
            self.assertEqual(sorted(words, key=len), ["a", "e", "bb", "dd", "ccc"])
            self.assertEqual(sorted(words, key=len, reverse=True), ["ccc", "bb", "dd", "a", "e"])
            self.assertEqual(sorted(items, key=operator.itemgetter(1)), [("y", 1), ("w", 1), ("z", 2), ("x", 3)])
            self.assertEqual(sorted(items, key=operator.itemgetter(1), reverse=True), [("x", 3), ("z", 2), ("y", 1), ("w", 1)])
            self.assertEqual([p.n for p in sorted(ps, key=operator.attrgetter("n"))], [1, 2, 3])
            ws = list(words)
            ws.sort(key=len)
            self.assertEqual(ws, ["a", "e", "bb", "dd", "ccc"])
            self.assertEqual(sorted(items, key=operator.itemgetter(1, 0)), [("w", 1), ("y", 1), ("z", 2), ("x", 3)])
        c = collections.Counter("abracadabra")
        self.assertEqual(c.most_common(3), [("a", 5), ("b", 2), ("r", 2)])
        with self.assertRaises(TypeError):
            sorted([1, 2], key=len)
        with self.assertRaises(TypeError):
            sorted([("a", 1), ("b", "x")], key=operator.itemgetter(1))
        with self.assertRaises(IndexError):
            sorted([(1,), ()], key=operator.itemgetter(0))
        self.assertEqual(sorted([[1, 2], [1], []], key=len), [[], [1], [1, 2]])

    def test_dicts_and_views(self):
        import collections
        import operator

        d = {"b": 2, "a": 3, "c": 1}
        for _ in range(200):
            self.assertEqual(sorted(d.items(), key=operator.itemgetter(1)), [("c", 1), ("b", 2), ("a", 3)])
            self.assertEqual(sorted(d.items(), key=operator.itemgetter(1), reverse=True), [("a", 3), ("b", 2), ("c", 1)])
            self.assertEqual(sorted(d, reverse=True), ["c", "b", "a"])
            self.assertEqual(sorted(d.keys(), key=None), ["a", "b", "c"])
            self.assertEqual(sorted(d.values(), reverse=True), [3, 2, 1])
            self.assertEqual(sorted(d.items(), reverse=True), [("c", 1), ("b", 2), ("a", 3)])
        c = collections.Counter("aabbbc")
        self.assertEqual(sorted(c.items(), key=operator.itemgetter(1), reverse=True), [("b", 3), ("a", 2), ("c", 1)])
        self.assertEqual(sorted({}.items(), reverse=True), [])
        with self.assertRaises(TypeError):
            sorted({1: "a", "x": "b"}, reverse=True)

    def test_sorted_returns_new_list(self):
        src = [3, 1, 2]
        out = sorted(src, reverse=True)
        self.assertEqual(src, [3, 1, 2])
        self.assertIsNot(out, src)
        out.append(0)
        self.assertEqual(src, [3, 1, 2])


if __name__ == "__main__":
    unittest.main()

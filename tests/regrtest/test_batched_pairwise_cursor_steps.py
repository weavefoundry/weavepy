"""itertools.batched and pairwise over list and tuple cursors drained from
compiled code: full and short last batches, strict batches, a source
iterator someone else holds (exhausted at the end, so later appends to
its list stay unseen, as in CPython), a batched stopped partway and
resumed, and zip_longest and pairwise leaving a shared source exhausted;
plus builtin types called with a spread (zip(*rows), tuple(*a)) from
compiled code."""

import itertools
import unittest


def drain(n, make):
    out = None
    for i in range(n):
        out = list(make())
    return out


class CursorStepTests(unittest.TestCase):
    def test_batched(self):
        xs = list(range(8))
        for n in (1, 300):
            self.assertEqual(drain(n, lambda: itertools.batched(xs, 3)), [(0, 1, 2), (3, 4, 5), (6, 7)])
            self.assertEqual(drain(n, lambda: itertools.batched(tuple(xs), 4)), [(0, 1, 2, 3), (4, 5, 6, 7)])
            self.assertEqual(drain(n, lambda: itertools.batched([], 2)), [])
            self.assertEqual(drain(n, lambda: itertools.batched(xs, 8, strict=True)), [tuple(xs)])
        with self.assertRaises(ValueError):
            drain(300, lambda: itertools.batched(xs, 3, strict=True))

    def test_batched_resume(self):
        for _ in range(300):
            b = itertools.batched("abcde", 2)
            self.assertEqual(next(b), ("a", "b"))
            self.assertEqual(list(b), [("c", "d"), ("e",)])
            self.assertEqual(list(b), [])

    def test_shared_source_exhausted(self):
        for _ in range(300):
            xs = [1, 2, 3, 4]
            it = iter(xs)
            self.assertEqual(list(itertools.batched(it, 3)), [(1, 2, 3), (4,)])
            xs.append(9)
            self.assertEqual(next(it, "end"), "end")
            ys = [1, 2, 3]
            it = iter(ys)
            self.assertEqual(list(itertools.pairwise(it)), [(1, 2), (2, 3)])
            ys.append(9)
            self.assertEqual(next(it, "end"), "end")
            zs = [1, 2]
            it = iter(zs)
            self.assertEqual(list(itertools.zip_longest(it, "abc")), [(1, "a"), (2, "b"), (None, "c")])
            zs.append(9)
            self.assertEqual(next(it, "end"), "end")

    def test_pairwise(self):
        xs = list(range(5))
        for n in (1, 300):
            self.assertEqual(drain(n, lambda: itertools.pairwise(xs)), [(0, 1), (1, 2), (2, 3), (3, 4)])
            self.assertEqual(drain(n, lambda: itertools.pairwise([1])), [])

    def test_spread_types(self):
        rows = [[1, 2, 3], [4, 5, 6]]
        a = ([1, 2],)
        for n in (1, 300):
            self.assertEqual(drain(n, lambda: zip(*rows)), [(1, 4), (2, 5), (3, 6)])
            self.assertEqual(drain(n, lambda: zip(*())), [])

            def spread(k):
                out = None
                for i in range(k):
                    out = (tuple(*a), list(*a), set(*a), dict(*[[("x", 1)]]), zip(*rows))
                return out

            t, l, s, d, z = spread(n)
            self.assertEqual((t, l, s, d, list(z)), ((1, 2), [1, 2], {1, 2}, {"x": 1}, [(1, 4), (2, 5), (3, 6)]))
        with self.assertRaises(TypeError):
            drain(300, lambda: zip(*[1, 2]))


if __name__ == "__main__":
    unittest.main()

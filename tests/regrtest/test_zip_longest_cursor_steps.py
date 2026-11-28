"""itertools.zip_longest over list and tuple cursors, drained by list() and
for loops from compiled code: fill values for sources of different
lengths, a source passed twice, generator and mixed sources, a source
that finishes while its list's last owner is the iterator (items with
__del__), a fresh source after one finished, and a zip_longest stopped
partway and resumed, each matching CPython's results, with every
finalizer run."""

import gc
import itertools
import unittest

log = []


class D:
    def __init__(self, n):
        self.n = n

    def __del__(self):
        log.append(self.n)


def drain(n, *srcs, **kw):
    out = None
    for i in range(n):
        out = list(itertools.zip_longest(*srcs, **kw))
    return out


class ZipLongestCursorTests(unittest.TestCase):
    def test_lengths_and_fill(self):
        a, b, c = [1, 2, 3], [4], ()
        for n in (1, 300):
            self.assertEqual(drain(n, a, b), [(1, 4), (2, None), (3, None)])
            self.assertEqual(drain(n, b, a, fillvalue="x"), [(4, 1), ("x", 2), ("x", 3)])
            self.assertEqual(drain(n, a, b, c), [(1, 4, None), (2, None, None), (3, None, None)])
            self.assertEqual(drain(n, c, c), [])
            self.assertEqual(drain(n, (1, 2), [3, 4]), [(1, 3), (2, 4)])

    def test_same_iterator_twice(self):
        for _ in range(300):
            it = iter([1, 2, 3, 4, 5])
            self.assertEqual(list(itertools.zip_longest(it, it)), [(1, 2), (3, 4), (5, None)])

    def test_mixed_sources(self):
        for _ in range(300):
            g = (x for x in "ab")
            self.assertEqual(
                list(itertools.zip_longest([1, 2, 3], g, (9,))),
                [(1, "a", 9), (2, "b", None), (3, None, None)],
            )

    def test_finalizers(self):
        for _ in range(50):
            del log[:]
            z = itertools.zip_longest(iter([D(1), D(2)]), iter([0, 0, 0]))
            got = [t[1] for t in z]
            gc.collect()
            self.assertEqual(got, [0, 0, 0])
            self.assertEqual(sorted(log), [1, 2])

    def test_resume(self):
        for _ in range(300):
            z = itertools.zip_longest([1, 2, 3], "ab")
            first = []
            for t in z:
                first.append(t)
                if len(first) == 2:
                    break
            self.assertEqual(first, [(1, "a"), (2, "b")])
            self.assertEqual(next(z), (3, None))
            self.assertEqual(list(z), [])
            with self.assertRaises(StopIteration):
                next(z)


if __name__ == "__main__":
    unittest.main()

"""`next()` of an `os.scandir` iterator.

`os.walk` steps its directory listing with `next(scandir_it)`, which runs
the iterator's own step in place: entries come out as iteration gives
them, the default (or `StopIteration`) at the end, and an iterator run to
its end counts as closed (no `ResourceWarning` when it's dropped).
"""

import gc
import os
import tempfile
import unittest
import warnings


class ScandirNextTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        for name in ("a", "b", "c"):
            open(os.path.join(self.dir, name), "w").close()
        os.mkdir(os.path.join(self.dir, "sub"))

    def tearDown(self):
        for name in ("a", "b", "c"):
            os.remove(os.path.join(self.dir, name))
        os.rmdir(os.path.join(self.dir, "sub"))
        os.rmdir(self.dir)

    def test_next_matches_iteration(self):
        for _ in range(500):
            with os.scandir(self.dir) as it:
                stepped = []
                while True:
                    try:
                        stepped.append(next(it).name)
                    except StopIteration:
                        break
            with os.scandir(self.dir) as it:
                iterated = [e.name for e in it]
        self.assertEqual(sorted(stepped), ["a", "b", "c", "sub"])
        self.assertEqual(sorted(stepped), sorted(iterated))

    def test_default_and_exhaustion(self):
        for _ in range(500):
            it = os.scandir(self.dir)
            names = [next(it).name for _ in range(4)]
            end = next(it, "done")
            again = next(it, None)
        self.assertEqual(len(names), 4)
        self.assertEqual((end, again), ("done", None))
        with self.assertRaises(StopIteration):
            next(it)

    def test_exhausted_iterator_does_not_warn(self):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            for _ in range(200):
                it = os.scandir(self.dir)
                while next(it, None) is not None:
                    pass
                del it
            gc.collect()
        self.assertEqual([w for w in caught if issubclass(w.category, ResourceWarning)], [])

    def test_walk(self):
        for _ in range(300):
            seen = list(os.walk(self.dir))
        self.assertEqual(len(seen), 2)
        top, dirs, files = seen[0]
        self.assertEqual((top, dirs, sorted(files)), (self.dir, ["sub"], ["a", "b", "c"]))
        self.assertEqual(seen[1], (os.path.join(self.dir, "sub"), [], []))

    def test_other_iterators_unchanged(self):
        class It:
            def __init__(self):
                self.n = 0

            def __iter__(self):
                return self

            def __next__(self):
                self.n += 1
                if self.n > 2:
                    raise StopIteration
                return self.n

        for _ in range(500):
            it = It()
            r = (next(it), next(it), next(it, "end"))
        self.assertEqual(r, (1, 2, "end"))


if __name__ == "__main__":
    unittest.main()

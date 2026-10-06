"""lru_cache keeps working when its wrapper's attributes move.

The wrapper reads its function, size, cache, and counters at known
places in its instance dictionary. Replacing `__wrapped__` after removing
it moves the entry to the end; the wrapper must still find every field.
"""

import functools
import unittest


class LruWrapperFieldsTest(unittest.TestCase):
    def test_counts_and_recency(self):
        @functools.lru_cache(maxsize=3)
        def sq(n):
            return n * n

        for i in range(10):
            self.assertEqual(sq(i % 4), (i % 4) ** 2)
        info = sq.cache_info()
        self.assertEqual(info.hits + info.misses, 10)
        self.assertEqual(info.currsize, 3)
        sq.cache_clear()
        self.assertEqual(sq.cache_info(), (0, 0, 3, 0))

    def test_moved_wrapped_entry(self):
        @functools.lru_cache(maxsize=8)
        def ident(n):
            return n

        for i in range(20):
            ident(i % 5)
        before = ident.cache_info()
        wrapped = vars(ident).pop("__wrapped__")
        ident.__wrapped__ = wrapped
        for i in range(20):
            self.assertEqual(ident(i % 5), i % 5)
        after = ident.cache_info()
        self.assertEqual(after.hits, before.hits + 20)
        self.assertEqual(after.misses, before.misses)

    def test_recursive_misses(self):
        @functools.lru_cache(maxsize=16)
        def collatz(n):
            if n == 1:
                return 1
            return 1 + collatz(n // 2 if n % 2 == 0 else 3 * n + 1)

        lengths = [collatz(1 + i % 50) for i in range(300)]
        self.assertEqual(lengths[26], 112)
        info = collatz.cache_info()
        self.assertEqual(info.currsize, 16)
        self.assertGreater(info.misses, info.hits)


if __name__ == "__main__":
    unittest.main()

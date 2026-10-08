"""`x in it` over the native lazy iterators.

`itertools` objects, `map`, `filter`, `zip` and `enumerate` have no
`__contains__`: membership consumes them, comparing each item with `==`,
as for any iterator (pydantic's MRO merge tests `c in islice(s, 1, None)`).
"""

import itertools
import unittest


class LazyIteratorMembershipTest(unittest.TestCase):
    def test_islice(self):
        self.assertIn(1, itertools.islice([0, 1, 2], 1, None))
        self.assertNotIn(0, itertools.islice([0, 1, 2], 1, None))

    def test_consumes_up_to_the_match(self):
        it = itertools.count()
        self.assertIn(3, it)
        self.assertEqual(next(it), 4)

    def test_adapters(self):
        self.assertIn(4, map(lambda x: x * 2, range(5)))
        self.assertIn(3, filter(None, [0, 3]))
        self.assertIn((1, "b"), zip([0, 1], "ab"))
        self.assertIn((1, "b"), enumerate("ab"))
        self.assertIn("c", itertools.chain("ab", "cd"))
        self.assertNotIn(9, itertools.repeat(1, 3))

    def test_hot_comprehension(self):
        seqs = [[1, 2, 3], [2, 3]]
        for _ in range(3000):
            hits = [s for s in seqs if 3 in itertools.islice(s, 1, None)]
        self.assertEqual(hits, seqs)


if __name__ == "__main__":
    unittest.main()

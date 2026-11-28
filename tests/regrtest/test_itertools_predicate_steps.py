"""`takewhile`, `dropwhile` and `filterfalse` step in place.

Over a list or tuple cursor with a predicate that runs as a pure leaf,
each adapter steps without the general path: `takewhile` stops at the
first failing item (consumed, as CPython consumes it), `dropwhile` skips
while the predicate holds, and `filterfalse` keeps the falsy verdicts.
Other predicates and sources keep the general step.
"""

import itertools
import unittest

N = 3000


class ItertoolsPredicateStepTests(unittest.TestCase):
    def test_takewhile(self):
        xs = [1, 2, 3, 4, 1]
        for _ in range(N):
            a = list(itertools.takewhile(lambda x: x < 3, xs))
        self.assertEqual(a, [1, 2])
        it = iter(xs)
        tw = itertools.takewhile(lambda x: x < 3, it)
        self.assertEqual(list(tw), [1, 2])
        self.assertEqual(list(it), [4, 1])
        self.assertEqual(list(tw), [])

    def test_dropwhile(self):
        xs = (1, 2, 5, 1, 7)
        for _ in range(N):
            d = list(itertools.dropwhile(lambda x: x < 3, xs))
        self.assertEqual(d, [5, 1, 7])
        self.assertEqual(list(itertools.dropwhile(lambda x: True, xs)), [])

    def test_filterfalse(self):
        xs = [0, 1, 2, 0, 3, ""]
        for _ in range(N):
            f = list(itertools.filterfalse(None, xs))
            g = list(itertools.filterfalse(lambda x: x == 2, [1, 2, 3]))
        self.assertEqual(f, [0, 0, ""])
        self.assertEqual(g, [1, 3])

    def test_side_effects_and_errors(self):
        seen = []

        def pred(x):
            seen.append(x)
            return x < 2

        self.assertEqual(list(itertools.takewhile(pred, [1, 2, 3])), [1])
        self.assertEqual(seen, [1, 2])
        with self.assertRaises(ZeroDivisionError):
            list(itertools.dropwhile(lambda x: 1 / x, [1, 0]))
        with self.assertRaises(TypeError):
            list(itertools.filterfalse(lambda x: x < "a", [1]))


if __name__ == "__main__":
    unittest.main()

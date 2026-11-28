"""`islice` over `chain` steps without the interpreter.

A `chain` whose current source is a list or tuple cursor, or whose next
source is a list or tuple with items, steps in place for a one-source
adapter (`islice`), moving to the next source as the general step does.
Sources it can't step (generators, iterators held elsewhere, empty
sequences) keep the general step, and the results match it.
"""

import unittest
from itertools import chain, islice

N = 3000


class ChainPureStepTests(unittest.TestCase):
    def test_sequence_sources(self):
        L = [1, 2, 3]
        for _ in range(N):
            a = list(islice(chain(L, L), 4))
            b = list(islice(chain([], (7,), [], [8, 9]), 5))
            c = list(islice(chain(L, "ab", range(2)), 9))
        self.assertEqual(a, [1, 2, 3, 1])
        self.assertEqual(b, [7, 8, 9])
        self.assertEqual(c, [1, 2, 3, "a", "b", 0, 1])

    def test_partial_reads_resume(self):
        for _ in range(N):
            c = chain([1], [2], [3])
            r = [list(islice(c, 2)), list(islice(c, 2))]
        self.assertEqual(r, [[1, 2], [3]])
        self.assertEqual(list(c), [])

    def test_shared_iterator_source(self):
        for _ in range(N):
            it = iter([5, 6])
            c = chain(it, [7])
            first = list(islice(c, 1))
            mid = next(it)
            rest = list(c)
        self.assertEqual((first, mid, rest), ([5], 6, [7]))

    def test_mutated_source(self):
        M = [1, 2]
        c = chain(M, M)
        self.assertEqual((next(c), next(c)), (1, 2))
        M.append(3)
        self.assertEqual(list(islice(c, 5)), [3, 1, 2, 3])

    def test_generator_and_from_iterable_sources(self):
        def g():
            yield 1
            yield 2

        for _ in range(N):
            a = list(islice(chain(g(), [3], g()), 10))
            b = list(islice(chain.from_iterable([[1], [], (2, 3)]), 10))
        self.assertEqual(a, [1, 2, 3, 1, 2])
        self.assertEqual(b, [1, 2, 3])

    def test_released_items_finalize(self):
        dead = []

        class D:
            def __del__(self):
                dead.append(1)

        def f():
            c = chain([D()], [1])
            return list(islice(c, 2))

        for _ in range(50):
            r = f()
            del r
        self.assertEqual(len(dead), 50)


if __name__ == "__main__":
    unittest.main()

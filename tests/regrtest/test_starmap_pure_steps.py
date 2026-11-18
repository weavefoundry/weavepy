"""`itertools.starmap` steps in place over a list or tuple of tuples.

With a callback that runs as a pure leaf (`operator.add`, `max`, a short
Python function), each tuple's fields are its arguments without the
general step; other items and callbacks keep the general step, and the
results and errors match it.
"""

import itertools
import operator
import unittest

N = 3000


def addmul(a, b, c=2):
    return (a + b) * c


class StarmapPureStepTests(unittest.TestCase):
    def test_native_and_python_callbacks(self):
        P = [(1, 2), (3, 4)]
        for _ in range(N):
            a = list(itertools.starmap(operator.add, P))
            b = list(itertools.starmap(max, ((1, 5, 2), (7, 0))))
            c = list(itertools.starmap(addmul, [(1, 2), (3, 4, 5)]))
        self.assertEqual(a, [3, 7])
        self.assertEqual(b, [5, 7])
        self.assertEqual(c, [6, 35])

    def test_non_tuple_items(self):
        for _ in range(N):
            r = list(itertools.starmap(operator.add, [(1, 2), [3, 4], "ab"]))
        self.assertEqual(r, [3, 7, "ab"])

    def test_errors_and_partial_reads(self):
        for _ in range(N):
            it = itertools.starmap(operator.truediv, [(1, 2), (1, 0), (3, 4)])
            first = next(it)
            with self.assertRaises(ZeroDivisionError):
                next(it)
            rest = list(it)
        self.assertEqual((first, rest), (0.5, [0.75]))
        with self.assertRaises(TypeError):
            list(itertools.starmap(operator.add, [(1,)]))

    def test_side_effects_in_order(self):
        seen = []

        def note(x, y):
            seen.append((x, y))
            return x

        list(itertools.starmap(note, [(1, 2), (3, 4)]))
        self.assertEqual(seen, [(1, 2), (3, 4)])


if __name__ == "__main__":
    unittest.main()

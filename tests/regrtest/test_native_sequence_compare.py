"""Comparing tuples and lists of plain values, natively.

Exact tuples or lists whose elements are machine ints, floats, `str`s or
such tuples compare without running any element's comparison through the
interpreter. Results must match CPython's sequence comparison: the first
unequal pair decides, identity counts as equality (a NaN equals itself),
lengths break ties, and anything else (mixed numbers, bools, instances)
compares as before.
"""

import heapq
import unittest

N = 2000


class Weird:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, Weird) and self.v == other.v

    def __lt__(self, other):
        return self.v > other.v


def reference(a, b):
    """The six comparisons of sequences `a` and `b`, element by element."""
    for x, y in zip(a, b):
        if x is y or x == y:
            continue
        if isinstance(x, (tuple, list)):
            lt, gt = reference(x, y)[0], reference(x, y)[2]
        else:
            lt, gt = x < y, x > y
        return (lt, lt, gt, gt, False, True)
    m, n = len(a), len(b)
    return (m < n, m <= n, m > n, m >= n, m == n, m != n)


class NativeSequenceCompareTest(unittest.TestCase):
    def test_orderings(self):
        nan = float("nan")
        cases = [
            ((1, 2), (1, 3)),
            ((1, (2, "a")), (1, (2, "b"))),
            ((1, 2), (1, 2, 0)),
            (("b",), ("a", "z")),
            ((nan,), (nan,)),
            ((float("nan"),), (float("nan"),)),
            ((1.5, 2), (1.5, 2)),
            ([1, 2, 3], [1, 2, 4]),
            ([(1, 2)], [(1, 2)]),
            ((1, 2.0), (1, 2)),
            ((True, 1), (1, True)),
            (((1,),), ((1, 0),)),
        ]
        for _ in range(N // 100):
            for a, b in cases:
                got = (a < b, a <= b, a > b, a >= b, a == b, a != b)
                self.assertEqual(got, reference(a, b), (a, b))
        self.assertTrue((nan,) == (nan,))
        self.assertFalse((float("nan"),) == (float("nan"),))
        self.assertFalse((float("nan"),) < (float("nan"),))
        self.assertTrue((1, 2) < (1, 2, 0))
        self.assertTrue([1, 2, 3] < [1, 2, 4])
        self.assertTrue((1, 2.0) == (1, 2))

    def test_fallback_elements(self):
        a, b = (1, Weird(1)), (1, Weird(2))
        self.assertTrue(a > b)
        self.assertTrue((Weird(1),) == (Weird(1),))
        with self.assertRaises(TypeError):
            (1, "a") < (1, 2)

    def test_heap_of_tuples(self):
        heap = []
        for i in range(N):
            heapq.heappush(heap, ((i * 7919) % 101, (i % 3, str(i))))
        out = [heapq.heappop(heap) for _ in range(N)]
        self.assertEqual(out, sorted(out))


if __name__ == "__main__":
    unittest.main()

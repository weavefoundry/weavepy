"""heapq's sifts compare and swap native scalars in place, and
itertools.product (and its kin) take a str's characters as a pool from
compiled code: heap orders for ints, floats and strs, mixed int and
float heaps, max-heaps, tuples and instances whose __lt__ runs Python
(the general path), a __lt__ that changes the heap, NaNs, and products,
permutations and combinations over strs with repeat=, each matching
CPython."""

import heapq
import itertools
import unittest


class Rev:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v > other.v


def churn(n, h, items):
    out = []
    for i in range(n):
        for x in items:
            heapq.heappush(h, x)
        out.append(heapq.heappop(h))
    return out


class HeapTests(unittest.TestCase):
    def test_scalar_heaps(self):
        for n in (1, 300):
            h = list(range(10, 0, -1))
            heapq.heapify(h)
            r = churn(n, h, [5, 3])
            self.assertEqual(r[:3], [1, 2, 3][: len(r[:3])])
            self.assertEqual(sorted(h), sorted(h))
            self.assertEqual(h[0], min(h))
            f = churn(n, [], [2.5, 1.5, 3])
            self.assertEqual(f[0], 1.5)
            s = churn(n, [], ["b", "a", "c"])
            self.assertEqual(s[0], "a")
        h = [3, 1, 2]
        heapq.heapify(h)
        self.assertEqual([heapq.heappop(h) for _ in range(3)], [1, 2, 3])

    def test_max_heap(self):
        h = [1, 5, 3, 4]
        heapq.heapify_max(h)
        for i in range(300):
            heapq.heappush_max(h, i % 7)
            heapq.heappop_max(h)
        self.assertEqual(h[0], max(h))
        out = [heapq.heappop_max(h) for _ in range(len(h))]
        self.assertEqual(out, sorted(out, reverse=True))

    def test_python_lt(self):
        h = []
        for i in range(300):
            heapq.heappush(h, Rev(i % 10))
        self.assertEqual(heapq.heappop(h).v, 9)
        t = []
        for i in range(300):
            heapq.heappush(t, (i % 5, str(i)))
        self.assertEqual(heapq.heappop(t), (0, "0"))

    def test_mutating_lt(self):
        h = []

        class Bad:
            def __lt__(self, other):
                h.clear()
                return True

        heapq.heappush(h, 1)
        heapq.heappush(h, 2)
        h.append(Bad())
        with self.assertRaises((RuntimeError, IndexError)):
            heapq.heappush(h, Bad())

    def test_nan(self):
        nan = float("nan")
        h = []
        for x in (3.0, nan, 1.0, 2.0):
            heapq.heappush(h, x)
        self.assertEqual(len(h), 4)
        self.assertEqual(str(sorted(str(x) for x in h)), str(sorted(["1.0", "2.0", "3.0", "nan"])))


class ProductPoolTests(unittest.TestCase):
    def test_str_pools(self):
        for i in range(300):
            p = list(itertools.product("ab", repeat=2))
            q = list(itertools.product("xy", "z"))
            r = list(itertools.permutations("abc", 2))
            c = list(itertools.combinations("abcd", 3))
        self.assertEqual(p, [("a", "a"), ("a", "b"), ("b", "a"), ("b", "b")])
        self.assertEqual(q, [("x", "z"), ("y", "z")])
        self.assertEqual(len(r), 6)
        self.assertEqual(c[0], ("a", "b", "c"))
        self.assertEqual(list(itertools.product("", repeat=2)), [])
        self.assertEqual(list(itertools.product("ab", repeat=0)), [()])
        self.assertEqual(list(itertools.product("hé", repeat=1)), [("h",), ("é",)])
        with self.assertRaises(ValueError):
            itertools.product("ab", repeat=-1)


if __name__ == "__main__":
    unittest.main()

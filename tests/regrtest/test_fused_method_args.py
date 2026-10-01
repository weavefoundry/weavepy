"""Fused `x.m(...)` calls whose argument is a small int expression.

The core loop runs `LOAD_FAST x; LOAD_ATTR m; <args>; CALL` as one step
for a local list, dict, set, str or native-method instance, folding an
argument like `i + 1` in place. These cases check the fold's edges:
overflow into big integers, non-int operands, and raising calls.
"""

import sys
import unittest
from collections import deque


class FusedArgumentTests(unittest.TestCase):
    def test_folded_int_arguments(self):
        q = deque()
        xs = []
        for i in range(5):
            q.append(i + 1)
            xs.append(i * 3)
            xs.append(i - 10)
            xs.append(i & 1)
            xs.append(i | 8)
            xs.append(i ^ 5)
        self.assertEqual(list(q), [1, 2, 3, 4, 5])
        self.assertEqual(xs[:6], [0, -10, 0, 8, 5, 3])

    def test_overflow_into_big_integers(self):
        big = sys.maxsize
        q = deque()
        xs = []
        for i in range(big - 1, big + 1):
            q.append(i + 1)
            xs.append(i * 2)
        self.assertEqual(list(q), [big, big + 1])
        self.assertEqual(xs, [(big - 1) * 2, big * 2])

    def test_non_int_operands(self):
        xs = []
        for x in (1.5, 2.5):
            xs.append(x + 1)
        s = "ab"
        for t in ("c", "d"):
            xs.append(s + t)
        for b in (True, False):
            xs.append(b + 1)
        self.assertEqual(xs, [2.5, 3.5, "abc", "abd", 2, 1])

    def test_raising_calls(self):
        q = deque()
        with self.assertRaises(IndexError):
            for i in range(3):
                q.pop()
        d = {}
        for i in range(3):
            self.assertIsNone(d.get(i + 1))
        with self.assertRaises(TypeError):
            for i in range(3):
                q.append(i + 1, 2)

    def test_maxlen_trim_with_folded_argument(self):
        ring = deque(maxlen=3)
        for i in range(10):
            ring.append(i + 100)
        self.assertEqual(list(ring), [107, 108, 109])


if __name__ == "__main__":
    unittest.main()

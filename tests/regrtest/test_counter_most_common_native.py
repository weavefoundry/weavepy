"""`Counter.most_common(n)` of plain int counts, natively.

An exact `Counter` whose counts are all ints (or bools) gives its `n`
largest `(key, count)` pairs as `heapq.nlargest(n, self.items(),
key=itemgetter(1))` does: largest first, equal counts in insertion order.
Other counts, subclasses, a non-int `n`, and any call a trace hook watches
take the Python method.
"""

import collections
import heapq
import operator
import sys
import unittest


def expected(c, n):
    return heapq.nlargest(n, c.items(), key=operator.itemgetter(1))


class MostCommonTests(unittest.TestCase):
    def test_orders(self):
        words = "the quick brown fox jumps over the lazy dog the fox".split()
        c = collections.Counter(words)
        for _ in range(100):
            for n in (0, 1, 2, 3, 5, 50, -1):
                self.assertEqual(c.most_common(n), expected(c, n))
            self.assertEqual(c.most_common(), sorted(c.items(), key=operator.itemgetter(1), reverse=True))
        ties = collections.Counter({"b": 2, "a": 2, "c": 2, "z": 3})
        self.assertEqual(ties.most_common(3), [("z", 3), ("b", 2), ("a", 2)])
        self.assertEqual(collections.Counter("abracadabra").most_common(3), [("a", 5), ("b", 2), ("r", 2)])
        neg = collections.Counter({"x": -1, "y": 0, "z": True})
        self.assertEqual(neg.most_common(2), expected(neg, 2))
        big = collections.Counter({i: i % 7 for i in range(500)})
        self.assertEqual(big.most_common(10), expected(big, 10))
        self.assertEqual(collections.Counter().most_common(3), [])

    def test_other_shapes(self):
        f = collections.Counter({"a": 1.5, "b": 2})
        self.assertEqual(f.most_common(1), [("b", 2)])

        class C(collections.Counter):
            def items(self):
                return [("only", 1)]

        self.assertEqual(C(a=3).most_common(1), [("only", 1)])
        c = collections.Counter("aab")
        with self.assertRaises(TypeError):
            c.most_common("2")
        self.assertEqual(c.most_common(True), [("a", 2)])

    def test_trace_sees_the_python_method(self):
        seen = []

        def tracer(frame, event, arg):
            if event == "call":
                seen.append(frame.f_code.co_name)
            return None

        c = collections.Counter("aabbc")
        sys.settrace(tracer)
        try:
            c.most_common(2)
        finally:
            sys.settrace(None)
        self.assertIn("nlargest", seen)


if __name__ == "__main__":
    unittest.main()

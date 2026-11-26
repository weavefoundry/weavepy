"""sum, min, max, any, all, list and tuple of iterables the interpreter
steps (generators), reached by the dispatcher's fast match: results,
start=, key=, default=, empty inputs, keyword errors, and a struct
sequence field named min, each matching CPython."""

import sys
import unittest


def g(n):
    yield from range(n)


class ConsumerDispatchTests(unittest.TestCase):
    def test_results(self):
        for i in range(300):
            r = (
                sum(x for x in g(4)), sum((x for x in g(3)), 10),
                min(x for x in g(4)), max(x for x in g(4)),
                min((x for x in g(4)), key=lambda v: -v),
                max((x for x in g(0)), default="none"),
                any(x > 2 for x in g(4)), all(x < 2 for x in g(4)),
                list(x for x in g(3)), tuple(x for x in g(2)),
            )
        self.assertEqual(r, (6, 13, 0, 3, 3, "none", True, False, [0, 1, 2], (0, 1)))

    def test_errors(self):
        with self.assertRaises(ValueError):
            min(x for x in g(0))
        with self.assertRaises(TypeError):
            any((x for x in g(1)), foo=1)
        with self.assertRaises(TypeError):
            sum(x for x in [1, "a"])
        self.assertEqual(sys.float_info.min, 2.2250738585072014e-308)
        self.assertEqual(max([3, 1], key=None), 3)


if __name__ == "__main__":
    unittest.main()

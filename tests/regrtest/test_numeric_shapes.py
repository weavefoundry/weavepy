"""Mixed int/float comparisons, powers, floor division and modulo from
hot loops, against their exact CPython semantics."""

import itertools
import unittest

VALUES = [0.0, -0.0, 1.0, -1.0, 2.5, -2.5, 3.0, -7.0, 0.5, 1e300, -1e300,
          1e-300, 7, -7, 2, 0, 3, 2**53, -(2**53), 2**53 + 1, float("inf"), float("nan")]


def outcome(op, a, b):
    try:
        r = op(a, b)
    except Exception as e:
        return type(e).__name__
    if isinstance(r, complex):
        return "complex"
    return repr(r)


class NumericShapesTest(unittest.TestCase):
    def test_against_reference(self):
        import operator

        ops = [operator.lt, operator.le, operator.eq, operator.ne, operator.gt,
               operator.floordiv, operator.mod, operator.pow]
        for a, b in itertools.product(VALUES, VALUES):
            for op in ops:
                if op is operator.pow and isinstance(a, int) and isinstance(b, int) and abs(b) > 64:
                    continue
                ref = outcome(op, a, b)
                # The same operation compiled inline, three times so the
                # interpreter's fast paths take over.
                for _ in range(3):
                    if op is operator.lt:
                        got = outcome(lambda x, y: x < y, a, b)
                    elif op is operator.le:
                        got = outcome(lambda x, y: x <= y, a, b)
                    elif op is operator.eq:
                        got = outcome(lambda x, y: x == y, a, b)
                    elif op is operator.ne:
                        got = outcome(lambda x, y: x != y, a, b)
                    elif op is operator.gt:
                        got = outcome(lambda x, y: x > y, a, b)
                    elif op is operator.floordiv:
                        got = outcome(lambda x, y: x // y, a, b)
                    elif op is operator.mod:
                        got = outcome(lambda x, y: x % y, a, b)
                    else:
                        got = outcome(lambda x, y: x ** y, a, b)
                    self.assertEqual(got, ref, (a, op.__name__, b))

    def test_overflow_messages(self):
        with self.assertRaises(OverflowError) as cm:
            2.5 ** 1e300
        self.assertEqual(cm.exception.args, (34, "Result too large"))
        with self.assertRaises(OverflowError) as cm:
            (-1e300) ** 2.5
        self.assertEqual(str(cm.exception), "complex exponentiation")
        self.assertEqual(2 ** 62, 4611686018427387904)
        self.assertEqual(2 ** 64, 18446744073709551616)
        self.assertEqual(2 ** -1, 0.5)


if __name__ == "__main__":
    unittest.main()

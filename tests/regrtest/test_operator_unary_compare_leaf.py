"""operator's unary and comparison functions, and float's reading methods,
called from compiled loops and as sort keys: plain ints and floats take the
in-place path, and everything else (a machine int's negation that
overflows, NaN, bools, big ints, str, instances with dunders, float
subclasses) gives the general call's value or error."""

import operator
import unittest


class Neg:
    def __neg__(self):
        return "neg"

    def __lt__(self, other):
        return "lt"


class F(float):
    def is_integer(self):
        return "mine"


def unary(n):
    out = None
    big = -(2**63)
    for i in range(n):
        out = (
            operator.neg(5),
            operator.neg(-2.5),
            operator.neg(big),
            operator.pos(7),
            operator.pos(-0.0),
            operator.invert(5),
            operator.inv(-1),
            operator.neg(True),
            operator.invert(False),
            operator.neg(2**70),
            operator.neg(Neg()),
        )
    return out


def compare(n):
    out = None
    nan = float("nan")
    for i in range(n):
        out = (
            operator.lt(1, 2),
            operator.le(2, 2),
            operator.eq(3, 3),
            operator.ne(3, 3),
            operator.gt(1.5, 0.5),
            operator.ge(0.5, 1.5),
            operator.lt(nan, 1.0),
            operator.ge(nan, nan),
            operator.eq(nan, nan),
            operator.ne(nan, nan),
            operator.lt(1, 2.5),
            operator.lt("a", "b"),
            operator.lt(Neg(), 1),
            operator.eq(True, 1),
        )
    return out


def floats(n):
    out = None
    for i in range(n):
        out = (
            (3.0).is_integer(),
            (3.5).is_integer(),
            float("inf").is_integer(),
            (0.75).as_integer_ratio(),
            (-2.0).hex(),
            (1.5).conjugate(),
            F(2.0).is_integer(),
        )
    return out


class OperatorLeafTests(unittest.TestCase):
    def test_unary(self):
        want = (-5, 2.5, 2**63, 7, -0.0, -6, 0, -1, -1, -(2**70), "neg")
        for n in (1, 500):
            got = unary(n)
            self.assertEqual(got, want)
            self.assertEqual(type(got[2]), int)
        self.assertEqual(str(unary(300)[4]), "-0.0")

    def test_compare(self):
        want = (True, True, True, False, True, False, False, False, False, True,
                True, True, "lt", True)
        for n in (1, 500):
            self.assertEqual(compare(n), want)

    def test_errors(self):
        def bad(n):
            caught = 0
            for i in range(n):
                try:
                    operator.neg("x")
                except TypeError:
                    caught += 1
                try:
                    operator.lt(1, "x")
                except TypeError:
                    caught += 1
            return caught

        self.assertEqual(bad(400), 800)
        with self.assertRaises(TypeError):
            operator.neg()
        with self.assertRaises(TypeError):
            operator.lt(1)

    def test_sort_keys(self):
        xs = [5, -3, 8, 0, 2**70, -(2**63)]
        for _ in range(300):
            self.assertEqual(sorted(xs, key=operator.neg),
                             [2**70, 8, 5, 0, -3, -(2**63)])
            ys = list(xs)
            ys.sort(key=operator.invert)
            self.assertEqual(ys, [2**70, 8, 5, 0, -3, -(2**63)])
        with self.assertRaises(TypeError):
            sorted([1, "a"], key=operator.neg)

    def test_floats(self):
        want = (True, False, False, (3, 4), "-0x1.0000000000000p+1", 1.5, "mine")
        for n in (1, 500):
            self.assertEqual(floats(n), want)
        with self.assertRaises(OverflowError):
            float("inf").as_integer_ratio()
        with self.assertRaises(ValueError):
            float("nan").as_integer_ratio()


if __name__ == "__main__":
    unittest.main()

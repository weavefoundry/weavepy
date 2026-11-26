"""list.index(x, start[, stop]) with int bounds scanned natively: found
and missing values, negative and out-of-range bounds, an empty range,
bools and big ints as bounds (the full path), instances whose __eq__
runs Python (the full path), and argument errors, each matching
CPython."""

import unittest


class Eq:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, Eq) and self.v == other.v


XS = [5, "a", 7, 5, 9, 5]


def find(n, *args):
    out = None
    for i in range(n):
        try:
            out = XS.index(*args)
        except ValueError as e:
            out = str(e)
    return out


class ListIndexTests(unittest.TestCase):
    def test_bounds(self):
        for n in (1, 300):
            self.assertEqual(find(n, 5), 0)
            self.assertEqual(find(n, 5, 1), 3)
            self.assertEqual(find(n, 5, 4), 5)
            self.assertEqual(find(n, 5, 1, 3), "list.index(x): x not in list")
            self.assertEqual(find(n, 5, -2), 5)
            self.assertEqual(find(n, 9, -100, 100), 4)
            self.assertEqual(find(n, "a", 2), "list.index(x): x not in list")
            self.assertEqual(find(n, 7, 3, 1), "list.index(x): x not in list")
            self.assertEqual(find(n, 5, True), 3)
            self.assertEqual(find(n, 5, 2**70), "list.index(x): x not in list")

    def test_python_eq_and_errors(self):
        ys = [Eq(1), Eq(2), Eq(1)]
        for i in range(300):
            r = ys.index(Eq(1), 1)
        self.assertEqual(r, 2)
        with self.assertRaises(TypeError):
            XS.index(5, "a")
        with self.assertRaises(TypeError):
            XS.index(5, 0, 1, 2)
        with self.assertRaises(TypeError):
            XS.index()


if __name__ == "__main__":
    unittest.main()

"""Frameless evaluations read `__slots__` members and compare tuples of
scalars and strings, as rich comparisons from sort and min call them;
results and every declined case match the full call."""

import unittest


class S:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x, self.y = x, y

    def __lt__(self, o):
        return self.x < o.x or (self.x == o.x and self.y < o.y)

    def __repr__(self):
        return f"S({self.x}, {self.y})"


class T:
    def __init__(self, *k):
        self.k = k

    def __lt__(self, o):
        return self.k < o.k

    def __eq__(self, o):
        return self.k == o.k

    def __le__(self, o):
        return self.k <= o.k


class Partial:
    __slots__ = ("x", "y")

    def __init__(self, x):
        self.x = x

    def __lt__(self, o):
        return self.y < o.y


class LeafSlotsTupleCompare(unittest.TestCase):
    def test_slot_sort(self):
        xs = [S(i * 7 % 13, i) for i in range(40)]
        want = sorted(xs, key=lambda s: (s.x, s.y))
        for _ in range(100):
            got = sorted(xs)
            self.assertEqual([(s.x, s.y) for s in got], [(s.x, s.y) for s in want])
            self.assertEqual(min(xs).x, 0)

    def test_tuple_compare(self):
        cases = [
            ((1, 2), (1, 3)), ((1, 2), (1, 2)), ((1,), (1, 0)), ((), ()),
            (("a", 1), ("a", 0)), ((1.5, "b"), (1.5, "a")), ((True, 1), (1, 0)),
            ((None, 1), (None, 2)), ((2, "x"), (1, "y")),
        ]
        for _ in range(100):
            for a, b in cases:
                ta, tb = T(*a), T(*b)
                self.assertEqual(ta < tb, a < b)
                self.assertEqual(ta == tb, a == b)
                self.assertEqual(ta <= tb, a <= b)

    def test_declined_cases(self):
        nan = float("nan")
        for _ in range(100):
            self.assertEqual(T(nan, 1) < T(nan, 2), (nan, 1) < (nan, 2))
            self.assertEqual(T(nan) == T(nan), (nan,) == (nan,))
            n = (nan,)
            self.assertTrue(T(*n) == T(*n))
            self.assertEqual(T((1, 2), 3) < T((1, 3), 0), True)
            with self.assertRaises(TypeError):
                T(1, "a") < T(1, 2)
            with self.assertRaises(TypeError):
                T(None, 1) < T(None, 2) if False else (T(None) < T(1))
            self.assertEqual(T([1]) < T([2]), True)

    def test_unset_slot_raises(self):
        a, b = Partial(1), Partial(2)
        for _ in range(100):
            with self.assertRaises(AttributeError):
                a < b
        a.y, b.y = 5, 3
        self.assertFalse(a < b)


if __name__ == "__main__":
    unittest.main()

"""Orderings answered by the reflected method.

A class that defines only `__lt__` answers `a > b` through `b.__lt__(a)`
(`object`'s `__gt__` answers `NotImplemented`); the comparison lanes call
the reflected method directly. The protocol around it must stay CPython's:
`NotImplemented` answers, subclass priority, `TypeError` when nothing
answers, and `min`/`max`/`sorted` over such instances.
"""

import functools
import unittest


class Lt:
    def __init__(self, x):
        self.x = x

    def __lt__(self, other):
        if not isinstance(other, Lt):
            return NotImplemented
        return self.x < other.x


class LtCounting:
    calls = 0

    def __init__(self, x):
        self.x = x

    def __lt__(self, other):
        LtCounting.calls += 1
        return self.x < other.x


class GtOverride(Lt):
    def __gt__(self, other):
        return "override"


class Plain:
    pass


class ReflectedOrderingTests(unittest.TestCase):
    ROUNDS = 4000

    def test_reflected_results(self):
        a, b = Lt(1), Lt(2)
        for _ in range(self.ROUNDS):
            self.assertFalse(a > b)
            self.assertTrue(b > a)
            self.assertEqual([a > b, b > a, a < b], [False, True, True])
        with self.assertRaises(TypeError):
            a >= b

    def test_each_comparison_calls_once(self):
        a, b = LtCounting(1), LtCounting(2)
        LtCounting.calls = 0
        for _ in range(self.ROUNDS):
            b > a
        self.assertEqual(LtCounting.calls, self.ROUNDS)

    def test_min_max_sorted(self):
        items = [Lt(i * 7 % 50) for i in range(50)]
        for _ in range(200):
            self.assertEqual(max(items).x, 49)
            self.assertEqual(min(items).x, 0)
        self.assertEqual([v.x for v in sorted(items)][:3], [0, 1, 2])
        self.assertEqual(max(items, key=lambda v: -v.x).x, 0)

    def test_not_implemented_and_errors(self):
        a = Lt(1)
        for _ in range(3):
            with self.assertRaises(TypeError):
                a > 1
            with self.assertRaises(TypeError):
                1 < a
            with self.assertRaises(TypeError):
                Plain() > Plain()
            with self.assertRaises(TypeError):
                Plain() <= Plain()

    def test_subclass_priority(self):
        a, s = Lt(1), GtOverride(5)
        for _ in range(self.ROUNDS):
            self.assertEqual(s > a, "override")
            self.assertEqual(a < s, "override")
            self.assertTrue(a > Lt(0))

    def test_class_gains_method(self):
        class C:
            def __init__(self, x):
                self.x = x

            def __lt__(self, other):
                return self.x < other.x

        a, b = C(1), C(2)
        for _ in range(self.ROUNDS):
            self.assertTrue(b > a)
        C.__gt__ = lambda self, other: "own"
        self.assertEqual(b > a, "own")
        del C.__gt__
        self.assertTrue(b > a)

    def test_total_ordering(self):
        @functools.total_ordering
        class T:
            def __init__(self, x):
                self.x = x

            def __eq__(self, other):
                return self.x == other.x

            def __lt__(self, other):
                return self.x < other.x

        for _ in range(self.ROUNDS):
            self.assertTrue(T(2) > T(1))
            self.assertTrue(T(2) >= T(2))
            self.assertFalse(T(1) >= T(2))


if __name__ == "__main__":
    unittest.main()

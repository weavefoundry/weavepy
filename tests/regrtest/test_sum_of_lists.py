"""`sum()` of lists.

After its first list item, `sum(lists, start)` extends the list it built
itself, which nothing else can reach, instead of building a new one per
item. The start list and the items are never changed, and the result is
a new list; an item of a list subclass with `__radd__`, a non-list item
and a generator source behave as CPython's repeated `+` does.
"""

import unittest


class SumOfListsTests(unittest.TestCase):
    def test_start_and_items_unchanged(self):
        start, a, b = [0], [1], [2, 3]
        for _ in range(1000):
            r = sum([a, b, a], start)
        self.assertEqual(r, [0, 1, 2, 3, 1])
        self.assertEqual((start, a, b), ([0], [1], [2, 3]))
        self.assertIsNot(r, start)

    def test_results_are_independent(self):
        x = [1]
        r1 = sum([x, x], [])
        r2 = sum([x, x], [])
        r1.append(9)
        self.assertEqual(r2, [1, 1])
        self.assertIsNot(r1, r2)

    def test_single_item_is_a_copy(self):
        a = [1, 2]
        r = sum([a], [])
        self.assertEqual(r, a)
        self.assertIsNot(r, a)

    def test_subclass_radd_and_add(self):
        class R(list):
            def __radd__(self, other):
                return "radd"

        class A(list):
            def __add__(self, other):
                return A(list.__add__(self, other))

        self.assertEqual(sum([[0], R([1])], []), "radd")
        r = sum([[2]], A([1]))
        self.assertIs(type(r), A)
        self.assertEqual(r, [1, 2])
        r = sum([[2], [3]], A([1]))
        self.assertIs(type(r), A)
        self.assertEqual(r, [1, 2, 3])

    def test_mixed_items_raise(self):
        with self.assertRaises(TypeError):
            sum([[1], (2,)], [])
        with self.assertRaises(TypeError):
            sum([[1], [2], 3], [])

    def test_generator_source(self):
        def g():
            for i in range(5):
                yield [i] * i

        for _ in range(1000):
            r = sum(g(), [])
        self.assertEqual(r, [1, 2, 2, 3, 3, 3, 4, 4, 4, 4])
        self.assertEqual(sum((x for x in []), [7]), [7])


if __name__ == "__main__":
    unittest.main()

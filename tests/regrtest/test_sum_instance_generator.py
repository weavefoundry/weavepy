"""`sum(obj)` where `iter(obj)` is a generator (a class whose `__iter__` is
a generator function, such as a tree walked with `yield from`) drains the
generator the way `sum(generator)` does, folding the yields it can in
place (see `do_sum_call` in the VM). Each case checks what plain
iteration gives: the total, `start`, mixed and float items, a raise
partway, PEP 479, and that `__iter__` runs exactly once.
"""

import unittest

WARM = 300


class Tree:
    __slots__ = ("left", "value", "right")

    def __init__(self, left, value, right):
        self.left = left
        self.value = value
        self.right = right

    def __iter__(self):
        if self.left:
            yield from self.left
        yield self.value
        if self.right:
            yield from self.right


def build(lo, hi):
    if lo >= hi:
        return None
    mid = (lo + hi) // 2
    return Tree(build(lo, mid), mid, build(mid + 1, hi))


class Items:
    def __init__(self, items):
        self.items = items
        self.iters = 0

    def __iter__(self):
        self.iters += 1
        for x in self.items:
            yield x


class Failing:
    def __iter__(self):
        yield 1
        yield 2
        raise KeyError("boom")


class LeakyStop:
    def __iter__(self):
        yield 1
        raise StopIteration


class Returning:
    def __iter__(self):
        yield 5
        return 99


class SumInstanceGeneratorTests(unittest.TestCase):
    def test_tree_warm(self):
        tree = build(0, 256)
        for _ in range(WARM):
            self.assertEqual(sum(tree), sum(range(256)))
        self.assertEqual(sum(tree, 1000), sum(range(256)) + 1000)
        self.assertEqual(sum(tree, start=-1), sum(range(256)) - 1)

    def test_iter_called_once(self):
        items = Items([1, 2, 3])
        self.assertEqual(sum(items), 6)
        self.assertEqual(items.iters, 1)

    def test_mixed_items(self):
        self.assertEqual(sum(Items([1, 2.5, True, 10**30])), 1 + 2.5 + 1 + 10**30)
        self.assertEqual(sum(Items([0.1] * 10)), sum([0.1] * 10))
        self.assertEqual(sum(Items([[1], [2]]), []), [1, 2])
        with self.assertRaises(TypeError):
            sum(Items([1, "a"]))

    def test_raise_partway(self):
        with self.assertRaises(KeyError):
            sum(Failing())

    def test_pep479(self):
        with self.assertRaises(RuntimeError) as cm:
            sum(LeakyStop())
        self.assertIsInstance(cm.exception.__cause__, StopIteration)

    def test_return_value_ignored(self):
        self.assertEqual(sum(Returning()), 5)

    def test_non_generator_iter(self):
        class ListIter:
            def __iter__(self):
                return iter([4, 5, 6])

        self.assertEqual(sum(ListIter()), 15)

    def test_empty(self):
        self.assertEqual(sum(Items([])), 0)
        self.assertEqual(sum(Items([]), 7), 7)


if __name__ == "__main__":
    unittest.main()

"""Generator expressions over closures and subscripts once their bodies
are compiled: fast steps run them natively (see `gen_fast` in the VM).

Each case warms a generator expression until its body compiles, then
checks a behavior at the boundary between the native steps and the
general loop: raises, unbound closure cells, exhausted iterators, and
finalizers of objects an exhausted iterator releases.
"""

import unittest

WARM = 3000


class Tracked:
    deleted = 0

    def __del__(self):
        Tracked.deleted += 1


def warm(fn, *args):
    for _ in range(WARM):
        fn(*args)


def tuple_of(pool, indices):
    return tuple(pool[i] for i in indices)


def set_of(vec, cols):
    return set(vec[i] + i for i in cols)


def sum_of(vec, cols):
    return sum(vec[i] * 2 for i in cols)


def attr_count(items, k):
    return sum(1 for w in items if w.kind is k)


class Item:
    __slots__ = ("kind",)

    def __init__(self, kind):
        self.kind = kind


class ClosureFastStepTests(unittest.TestCase):
    def test_results(self):
        pool = (10, 11, 12, 13)
        warm(tuple_of, pool, [3, 2, 1, 0])
        self.assertEqual(tuple_of(pool, [3, 2, 1, 0]), (13, 12, 11, 10))
        self.assertEqual(tuple_of(pool, []), ())
        vec = (3, 1, 0, 2)
        warm(set_of, vec, range(4))
        self.assertEqual(set_of(vec, range(4)), {3, 2, 2, 5})
        warm(sum_of, vec, range(4))
        self.assertEqual(sum_of(vec, range(4)), 12)
        items = [Item(i % 3) for i in range(10)]
        warm(attr_count, items, 1)
        self.assertEqual([attr_count(items, k) for k in (0, 1, 2)], [4, 3, 3])

    def test_raise_after_warm_up(self):
        pool = (1, 2, 3)
        warm(tuple_of, pool, [0, 1, 2])
        with self.assertRaises(IndexError):
            tuple_of(pool, [0, 1, 7])
        gen = (pool[i] for i in [0, 9])
        self.assertEqual(next(gen), 1)
        with self.assertRaises(IndexError):
            next(gen)
        # The generator finished with the raise.
        self.assertEqual(list(gen), [])
        with self.assertRaises(TypeError):
            tuple_of(pool, [0, "x"])

    def test_unbound_cell(self):
        def make():
            v = (1, 2, 3)
            gen = (v[i] for i in range(3))
            first = next(gen)
            del v
            return first, gen

        for _ in range(WARM):
            v = (1, 2)
            list(v[i] for i in range(2))
        first, gen = make()
        self.assertEqual(first, 1)
        with self.assertRaises(NameError):
            next(gen)

    def test_shared_tuple_iterator(self):
        def drain(it):
            return list(x + 1 for x in it)

        for _ in range(WARM):
            drain(iter((1, 2, 3)))
        it = iter((1, 2, 3))
        self.assertEqual(drain(it), [2, 3, 4])
        self.assertEqual(next(it, "end"), "end")
        self.assertEqual(list(it), [])

    def test_shared_list_iterator_detaches(self):
        def drain(it):
            return list(x * 2 for x in it)

        for _ in range(WARM):
            drain(iter([1, 2]))
        lst = [1, 2, 3]
        it = iter(lst)
        self.assertEqual(drain(it), [2, 4, 6])
        lst.append(4)
        self.assertEqual(next(it, "end"), "end")
        # A generator expression's own `.0` over a list that grows later.
        lst = [5, 6]
        gen = (x for x in lst)
        self.assertEqual(list(gen), [5, 6])
        lst.append(7)
        self.assertEqual(list(gen), [])

    def test_exhausted_tuple_releases_items(self):
        def count(a, b):
            return sum(1 for x in (a, b))

        for _ in range(WARM):
            count(1, 2)
        Tracked.deleted = 0
        n = count(Tracked(), Tracked())
        # The iterator's tuple held the last references.
        self.assertEqual(n, 2)
        self.assertEqual(Tracked.deleted, 2)

        def first_only(a, b):
            for x in (y for y in (a, b)):
                return x

        Tracked.deleted = 0
        for _ in range(WARM):
            first_only(1, 2)
        first_only(Tracked(), Tracked())
        self.assertEqual(Tracked.deleted, 2)

    def test_nested_closure_generators(self):
        def outer(xs, k):
            inner = (x * k for x in xs)
            return [y + k for y in (v for v in inner)]

        warm(outer, (1, 2, 3), 2)
        self.assertEqual(outer((1, 2, 3), 2), [4, 6, 8])


if __name__ == "__main__":
    unittest.main()

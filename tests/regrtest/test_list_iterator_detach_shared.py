"""Exhausted list and reverse iterators detach from their lists through a
shared empty store; loops over small lists and tuples retire their cursors
in place."""

import gc
import pickle
import unittest


class Tracked:
    deleted = 0

    def __del__(self):
        Tracked.deleted += 1


class DetachTests(unittest.TestCase):
    def test_exhausted_iterators_stay_empty(self):
        a = [1, 2]
        b = [3]
        ia, ib = iter(a), iter(b)
        self.assertEqual(list(ia), [1, 2])
        self.assertEqual(list(ib), [3])
        a.append(9)
        b.append(9)
        self.assertEqual(list(ia), [])
        self.assertEqual(list(ib), [])
        # A fresh iterator over a list still sees everything.
        self.assertEqual(list(iter(a)), [1, 2, 9])
        r = reversed([1, 2])
        self.assertEqual(list(r), [2, 1])
        self.assertEqual(list(r), [])

    def test_reduce_after_exhaustion(self):
        it = iter([1, 2, 3])
        list(it)
        f, args = it.__reduce__()[:2]
        self.assertEqual(args, ([],))
        args[0].append(5)
        it2 = iter([4])
        list(it2)
        self.assertEqual(it2.__reduce__()[1], ([],))
        self.assertEqual(list(pickle.loads(pickle.dumps(it))), [])
        r = reversed([1])
        list(r)
        self.assertEqual(r.__reduce__()[1], ([],))

    def test_shared_generator_cursor_detaches(self):
        xs = [1, 2]
        g = (x for x in xs)
        self.assertEqual(list(g), [1, 2])
        xs.append(3)
        self.assertEqual(list(g), [])

    def test_small_loops(self):
        g = {0: [1, 2], 1: [3], 2: []}
        total = 0
        for i in range(3000):
            for w in g[i % 3]:
                total += w
        self.assertEqual(total, 6000)
        t = (1, 2)
        empty = []
        n = 0
        for i in range(3000):
            for w in t:
                n += w
            for w in empty:
                n += 100
        self.assertEqual(n, 9000)

    def test_temporary_list_frees_items_at_loop_end(self):
        Tracked.deleted = 0
        for i in range(500):
            for x in [Tracked(), Tracked()]:
                pass
        del x
        gc.collect()
        self.assertEqual(Tracked.deleted, 1000)

    def test_iterators_collected(self):
        Tracked.deleted = 0
        its = []
        for i in range(200):
            it = iter([Tracked()])
            list(it)
            its.append(it)
        gc.collect()
        self.assertEqual(Tracked.deleted, 200)
        del its
        gc.collect()


if __name__ == "__main__":
    unittest.main()

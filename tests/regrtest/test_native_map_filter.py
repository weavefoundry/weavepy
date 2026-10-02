"""`map` and `filter` over native containers, stepped natively.

The common shapes are built and stepped without running the lazy
classes' Python methods. Laziness, error timing, type identity and
pickling must stay exactly as with the classes' own methods.
"""

import pickle
import unittest


class NativeMapFilter(unittest.TestCase):
    def test_results(self):
        data = list(range(20))
        self.assertEqual(list(map(str, data[:5])), ["0", "1", "2", "3", "4"])
        self.assertEqual(sum(map(abs, [-1, -2, 3])), 6)
        self.assertEqual(list(map(lambda x: x * 2, (1, 2, 3))), [2, 4, 6])
        self.assertEqual("".join(map(str.upper, "abc")), "ABC")
        self.assertEqual(list(map(lambda a, b: a + b, [1, 2], [10, 20, 30])), [11, 22])
        self.assertEqual(list(filter(None, [0, 1, "", "x", None])), [1, "x"])
        self.assertEqual(list(filter(bool, [0, 2, 0.0, 3])), [2, 3])
        self.assertEqual(list(filter(lambda x: x % 3 == 0, range(10))), [0, 3, 6, 9])
        total = 0
        for v in map(lambda x: x + 1, data):
            total += v
        self.assertEqual(total, sum(data) + len(data))

    def test_type_identity(self):
        m = map(str, [1])
        f = filter(None, [1])
        # (WeavePy's `map` and `filter` names are functions building these
        # classes, so the types are compared with each other.)
        self.assertIs(type(m), type(map(abs, iter([]))))
        self.assertIs(type(f), type(filter(abs, iter([]))))
        self.assertIs(iter(m), m)
        self.assertEqual(type(m).__name__, "map")
        self.assertEqual(type(f).__name__, "filter")

    def test_lazy_errors(self):
        def boom(x):
            if x == 2:
                raise ValueError("two")
            return x

        it = map(boom, [1, 2, 3])
        self.assertEqual(next(it), 1)
        with self.assertRaises(ValueError):
            next(it)
        self.assertEqual(next(it), 3)
        with self.assertRaises(StopIteration):
            next(it)
        fi = filter(boom, [1, 2])
        self.assertEqual(next(fi), 1)
        with self.assertRaises(ValueError):
            next(fi)

    def test_shared_iterator(self):
        src = iter([1, 2, 3, 4])
        m = map(lambda x: x * 10, src)
        self.assertEqual(next(m), 10)
        self.assertEqual(next(src), 2)
        self.assertEqual(list(m), [30, 40])

    def test_pickle_round_trip(self):
        m = map(abs, [-1, -2, -3])
        next(m)
        self.assertEqual(list(pickle.loads(pickle.dumps(m))), [2, 3])
        f = filter(None, [0, 5, 0, 6])
        self.assertEqual(list(pickle.loads(pickle.dumps(f))), [5, 6])

    def test_side_effect_order(self):
        seen = []

        def note(x):
            seen.append(x)
            return x

        m = map(note, range(5))
        self.assertEqual(seen, [])
        next(m)
        next(m)
        self.assertEqual(seen, [0, 1])


if __name__ == "__main__":
    unittest.main()

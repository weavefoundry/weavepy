"""`list(filter(None, xs))` and `filter(bool, xs)` collected in one call.

Lists and tuples of ints, floats, strings, `None` and bools give the
truthy items in order, from the start or after some were taken; the
filter and its source iterator are exhausted afterwards; an item with its
own `__bool__` anywhere among the rest is tested through it, in order;
and a source iterator shared with other code ends up where stepping it
item by item leaves it.
"""

import gc
import unittest
import weakref


class Truth:
    calls = []

    def __init__(self, v):
        self.v = v

    def __bool__(self):
        Truth.calls.append(self.v)
        return bool(self.v)


class FilterCollectTests(unittest.TestCase):
    def test_plain(self):
        xs = [0, 1, 2, 0, -3, 0.0, 1.5, "", "a", None, True, False] * 50
        want = [x for x in xs if x]
        self.assertEqual(list(filter(None, xs)), want)
        self.assertEqual(list(filter(bool, xs)), want)
        self.assertEqual(list(filter(None, tuple(xs))), want)
        self.assertEqual(sorted(filter(None, xs), key=str), sorted(want, key=str))
        self.assertEqual(list(filter(None, [])), [])
        self.assertEqual(list(filter(None, [0, 0, None, ""])), [])

    def test_after_some_taken(self):
        xs = list(range(10))
        f = filter(None, xs)
        self.assertEqual(next(f), 1)
        self.assertEqual(next(f), 2)
        self.assertEqual(list(f), list(range(3, 10)))
        self.assertEqual(list(f), [])
        with self.assertRaises(StopIteration):
            next(f)

    def test_shared_source(self):
        xs = [0, 1, 2, 3]
        it = iter(xs)
        f = filter(None, it)
        self.assertEqual(list(f), [1, 2, 3])
        self.assertEqual(list(it), [])
        xs.append(4)
        # An exhausted list iterator stays exhausted.
        self.assertEqual(list(it), [])
        self.assertEqual(list(f), [])

    def test_bool_items(self):
        Truth.calls = []
        xs = [1, Truth(0), 2, Truth(5), 0]
        self.assertEqual([x if isinstance(x, int) else x.v for x in filter(None, xs)],
                         [1, 2, 5])
        self.assertEqual(Truth.calls, [0, 5])

    def test_bool_error(self):
        class Bad:
            def __bool__(self):
                raise ValueError("no")

        xs = [1, 2, Bad(), 3]
        f = filter(None, xs)
        with self.assertRaises(ValueError):
            list(f)
        self.assertEqual(list(f), [3])

    def test_list_released(self):
        class Box(list):
            pass

        xs = Box([0, 1, 2])
        r = weakref.ref(xs)
        f = filter(None, iter(xs))
        del xs
        self.assertEqual(list(f), [1, 2])
        gc.collect()
        self.assertIsNone(r())

    def test_many(self):
        xs = list(range(-500, 500)) * 3
        for _ in range(300):
            self.assertEqual(len(list(filter(None, xs))), 2997)


if __name__ == "__main__":
    unittest.main()

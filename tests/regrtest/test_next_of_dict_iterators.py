"""`next()` of a dict's or dict view's iterator, and `iter()` of a view.

`next(iter(d))` steps a dict iterator in place, as a `for` loop does; a
dict changed in size mid-iteration, and the end (with or without a
default), are still the full call's to report, with CPython's errors.
`iter()` of a view or a `mappingproxy` makes its iterator directly.
"""

import types
import unittest


class NextOfDictIteratorTests(unittest.TestCase):
    def test_first_items(self):
        d = {"a": 1, "b": 2, "c": 3}
        for _ in range(300):
            self.assertEqual(next(iter(d)), "a")
            self.assertEqual(next(iter(d.keys())), "a")
            self.assertEqual(next(iter(d.values())), 1)
            self.assertEqual(next(iter(d.items())), ("a", 1))
            self.assertEqual(next(iter(d.values()), None), 1)
            self.assertEqual(next(iter({}), "empty"), "empty")

    def test_steps_and_end(self):
        d = {i: str(i) for i in range(5)}
        for _ in range(100):
            it = iter(d.items())
            got = [next(it) for _ in range(5)]
            self.assertEqual(got, list(d.items()))
            self.assertIsNone(next(it, None))
            with self.assertRaises(StopIteration):
                next(it)
            # (An exhausted iterator stays exhausted after the dict grows.)
            d[99] = "x"
            self.assertIsNone(next(it, None))
            del d[99]

    def test_mutation(self):
        for _ in range(100):
            d = {1: 1, 2: 2}
            it = iter(d)
            self.assertEqual(next(it), 1)
            d[3] = 3
            with self.assertRaisesRegex(RuntimeError, "dictionary changed size during iteration"):
                next(it)
            d = {1: 1, 2: 2}
            it = iter(d.values())
            next(it)
            d[2] = "changed"
            self.assertEqual(next(it), "changed")

    def test_heap_keys_and_values(self):
        d = {("k", i): [i] for i in range(4)}
        for _ in range(100):
            k = next(iter(d))
            self.assertEqual(k, ("k", 0))
            v = next(iter(d.values()))
            v.append(0)
            v.pop()
            self.assertEqual(d[("k", 0)], [0])

    def test_views_and_proxies(self):
        d = {"x": 1, "y": 2}
        proxy = types.MappingProxyType(d)
        for _ in range(300):
            self.assertEqual(list(iter(d.keys())), ["x", "y"])
            self.assertEqual(list(iter(d.items())), [("x", 1), ("y", 2)])
            self.assertEqual(list(iter(proxy)), ["x", "y"])
            self.assertEqual(list(iter(d.values())), [1, 2])
        ns = type("C", (), {"q": 1}).__dict__
        self.assertEqual(next(iter(ns)), list(ns)[0])

    def test_subclass(self):
        class D(dict):
            def __iter__(self):
                yield "own"

        d = D(a=1)
        for _ in range(100):
            self.assertEqual(next(iter(d)), "own")
            self.assertEqual(next(iter(d.keys())), "a")


if __name__ == "__main__":
    unittest.main()

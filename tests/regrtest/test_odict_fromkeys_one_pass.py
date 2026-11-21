"""`OrderedDict.fromkeys` of a list, tuple or str in one native pass.

An exact `OrderedDict` takes the keys through `dict.fromkeys` and its
native update; the order (first occurrences), the shared value, subclasses'
own `__setitem__`, other iterables and CPython's wording for an unhashable
key must all stay as the per-key loop has them.
"""

import unittest
from collections import OrderedDict


class Tagged(OrderedDict):
    def __setitem__(self, key, value):
        super().__setitem__(key, ("tagged", value))


class FromkeysTests(unittest.TestCase):
    def test_order_and_values(self):
        words = ["b", "a", "c", "a", 1, 2.0, (1, 2), "b"]
        for _ in range(300):
            od = OrderedDict.fromkeys(words, 0)
            self.assertEqual(list(od.items()), [("b", 0), ("a", 0), ("c", 0), (1, 0), (2.0, 0), ((1, 2), 0)])
            self.assertIs(type(od), OrderedDict)
        shared = []
        od = OrderedDict.fromkeys(("x", "y"), shared)
        self.assertIs(od["x"], od["y"])
        self.assertEqual(list(OrderedDict.fromkeys("hello")), ["h", "e", "l", "o"])
        self.assertEqual(OrderedDict.fromkeys([]), OrderedDict())

    def test_order_operations_after(self):
        od = OrderedDict.fromkeys("abcd")
        od.move_to_end("a")
        self.assertEqual(list(od), ["b", "c", "d", "a"])
        self.assertEqual(od.popitem(), ("a", None))
        self.assertEqual(od.popitem(last=False), ("b", None))
        od["z"] = 1
        self.assertEqual(list(od), ["c", "d", "z"])

    def test_subclass_setitem(self):
        od = Tagged.fromkeys(["a", "b"], 1)
        self.assertIs(type(od), Tagged)
        self.assertEqual(list(od.items()), [("a", ("tagged", 1)), ("b", ("tagged", 1))])

    def test_other_iterables(self):
        self.assertEqual(list(OrderedDict.fromkeys(x for x in "abca")), ["a", "b", "c"])
        self.assertEqual(list(OrderedDict.fromkeys({"q": 1, "r": 2})), ["q", "r"])
        self.assertEqual(list(OrderedDict.fromkeys(range(3))), [0, 1, 2])

    def test_unhashable(self):
        for bad in ([[1]], ([1], 2), [1, [2]]):
            with self.assertRaisesRegex(TypeError, "^unhashable type: 'list'$"):
                OrderedDict.fromkeys(bad)


if __name__ == "__main__":
    unittest.main()

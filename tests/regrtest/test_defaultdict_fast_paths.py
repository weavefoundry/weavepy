"""`defaultdict` construction and misses in place.

`defaultdict()` and `defaultdict(factory)` of the exact class build the
instance directly; a miss whose factory is a common builtin type stores
and returns the new value as `__missing__` does. Other factories, a
`None` factory, subclasses with their own `__missing__`, and bad
factories keep the Python path and its errors.
"""

import copy
import pickle
import unittest
from collections import defaultdict

N = 3000


class OwnMissing(defaultdict):
    def __missing__(self, key):
        return "mine"


class DefaultdictFastPathTests(unittest.TestCase):
    def test_construction(self):
        for _ in range(N):
            a = defaultdict()
            b = defaultdict(list)
            c = defaultdict(int, {"x": 1})
        self.assertIsNone(a.default_factory)
        self.assertIs(b.default_factory, list)
        self.assertEqual(c, {"x": 1})
        self.assertEqual(repr(b), "defaultdict(<class 'list'>, {})")
        with self.assertRaises(TypeError):
            defaultdict(1)

    def test_misses(self):
        for _ in range(N):
            d = defaultdict(list)
            d["x"].append(1)
            d["x"].append(2)
            e = defaultdict(int)
            e["a"] += 1
            e["a"] += 1
            f = defaultdict(lambda: "z")
            f["q"]
        self.assertEqual((d, e, f), ({"x": [1, 2]}, {"a": 2}, {"q": "z"}))
        self.assertEqual(list(d), ["x"])

    def test_none_factory_and_subclass(self):
        g = defaultdict(None)
        for _ in range(100):
            with self.assertRaises(KeyError):
                g["k"]
        m = OwnMissing(list)
        for _ in range(N):
            r = m["a"]
        self.assertEqual((r, dict(m)), ("mine", {}))

    def test_round_trips(self):
        b = defaultdict(list)
        b["k"].append(1)
        self.assertEqual(pickle.loads(pickle.dumps(b)), b)
        self.assertEqual(copy.deepcopy(b), b)
        self.assertIs(type(copy.copy(b)), defaultdict)


if __name__ == "__main__":
    unittest.main()

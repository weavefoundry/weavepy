"""`key in ChainMap` over exact dicts decided natively (its native
__contains__), and itertools.chain draining str sources in bulk, from
compiled loops: keys in the first, a later, or no map, an int key, keys
with a Python __hash__/__eq__ (the full path), maps that aren't dicts,
a ChainMap subclass overriding __contains__, maps changed after warm-up,
and chains of lists, tuples, strs (non-ASCII too) and empty strs, each
matching CPython."""

import collections
import itertools
import unittest


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and self.v == other.v


class Never(collections.ChainMap):
    def __contains__(self, key):
        return False


class ChainMapTests(unittest.TestCase):
    def test_contains(self):
        cm = collections.ChainMap({"a": 1}, {"b": 2, 3: 4})
        for i in range(300):
            r = ("a" in cm, "b" in cm, 3 in cm, "z" in cm, 4 in cm)
        self.assertEqual(r, (True, True, True, False, False))
        k = collections.ChainMap({Key(1): 1}, {})
        for i in range(300):
            r2 = (Key(1) in k, Key(2) in k)
        self.assertEqual(r2, (True, False))
        u = collections.ChainMap({"a": 1}, collections.UserDict({"q": 1}))
        for i in range(300):
            r3 = ("q" in u, "x" in u)
        self.assertEqual(r3, (True, False))
        n = Never({"a": 1})
        for i in range(300):
            r4 = "a" in n
        self.assertFalse(r4)
        cm.maps[1]["new"] = 0
        self.assertIn("new", cm)
        cm.maps.append({"late": 1})
        self.assertIn("late", cm)

    def test_chain_str(self):
        for i in range(300):
            r = list(itertools.chain([1], (2,), "ab", "", "hé"))
            s = list(itertools.chain.from_iterable(["xy", [3]]))
        self.assertEqual(r, [1, 2, "a", "b", "h", "é"])
        self.assertEqual(s, ["x", "y", 3])


if __name__ == "__main__":
    unittest.main()

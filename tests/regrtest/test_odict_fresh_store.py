"""`OrderedDict` stores and construction from pairs.

A fresh `OrderedDict`'s first `od[key] = value` starts its order in place,
as the full store does; construction from an iterable unpacks 2-tuples
directly and anything else as CPython's `odict` does, with its errors.
"""

import unittest
from collections import OrderedDict

N = 3000


class S(OrderedDict):
    def __setitem__(self, k, v):
        super().__setitem__(k, v * 10)


class OdictFreshStoreTests(unittest.TestCase):
    def test_fresh_stores_keep_order(self):
        for _ in range(N):
            o = OrderedDict()
            o["b"] = 1
            o["a"] = 2
            o["b"] = 3
        self.assertEqual(list(o.items()), [("b", 3), ("a", 2)])
        o.move_to_end("b")
        self.assertEqual(list(o), ["a", "b"])
        del o["a"]
        self.assertEqual(list(o), ["b"])

    def test_construction_from_pairs(self):
        P = [("a", 1), ("b", 2)]
        for _ in range(N):
            g = OrderedDict((k, v) for k, v in P)
            m = OrderedDict(iter([("x", 1), ["y", 2], "zq"]))
        self.assertEqual(list(g.items()), P)
        self.assertEqual(list(m.items()), [("x", 1), ("y", 2), ("z", "q")])

    def test_pair_errors(self):
        for bad, msg in [([()], "need more than 0"), ([(1,)], "need more than 1"),
                         ([(1, 2, 3)], "too many values")]:
            with self.assertRaises(ValueError) as cm:
                OrderedDict(iter(bad))
            self.assertIn(msg, str(cm.exception))

    def test_subclass_setitem(self):
        for _ in range(N):
            s = S(iter([("a", 1)]))
            s["b"] = 2
        self.assertEqual(dict(s), {"a": 10, "b": 20})

    def test_unhashable_key(self):
        o = OrderedDict()
        with self.assertRaises(TypeError):
            o[[1]] = 2
        self.assertEqual(list(o), [])


if __name__ == "__main__":
    unittest.main()

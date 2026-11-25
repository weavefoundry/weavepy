"""defaultdict misses filled in compiled loops while the class keeps the
native __missing__ (a verdict now remembered per class version): counts,
lists and sets as factories, a factory replaced partway, no factory, a
Python factory, a subclass that overrides __missing__, one that gains an
override after the loop warmed up, and get() not inserting, each
matching CPython."""

import collections
import unittest


def count(n, d, words):
    for i in range(n):
        for w in words:
            d[w] += 1
    return d


class DefaultDictMissingTests(unittest.TestCase):
    def test_factories(self):
        words = "a b a c b a".split()
        for n in (1, 300):
            d = count(n, collections.defaultdict(int), words)
            self.assertEqual(dict(d), {"a": 3 * n, "b": 2 * n, "c": n})
            g = collections.defaultdict(list)
            for i in range(n):
                g[i % 3].append(i)
            self.assertEqual(sorted(g), [0, 1, 2][: min(n, 3)])
            s = collections.defaultdict(set)
            for i in range(n):
                s["k"].add(i % 2)
            self.assertEqual(s["k"], {0, 1} if n > 1 else {0})

    def test_factory_changes(self):
        d = collections.defaultdict(int)
        count(300, d, ["x"])
        d.default_factory = lambda: 100
        count(300, d, ["y"])
        self.assertEqual(d["y"], 400)
        d.default_factory = None
        with self.assertRaises(KeyError):
            count(300, d, ["z"])
        self.assertEqual(d.get("w"), None)
        self.assertNotIn("w", d)

    def test_subclass_override(self):
        class Loud(collections.defaultdict):
            def __missing__(self, key):
                self[key] = 10
                return 10

        d = count(300, Loud(int), ["q"])
        self.assertEqual(d["q"], 310)

        class Later(collections.defaultdict):
            pass

        d = count(300, Later(int), ["r"])
        self.assertEqual(d["r"], 300)
        Later.__missing__ = lambda self, key: self.__setitem__(key, 50) or 50
        d2 = count(300, Later(int), ["r"])
        self.assertEqual(d2["r"], 350)


if __name__ == "__main__":
    unittest.main()

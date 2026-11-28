"""dict() of generators, iterators and zips of pairs from compiled loops:
insertion order, duplicate keys (last value wins, first position kept),
list and string pairs, a pair of the wrong length, an unhashable key,
keys whose __eq__ and __hash__ are Python code, and a generator that
raises partway, each matching CPython."""

import unittest


class K:
    def __init__(self, k):
        self.k = k

    def __eq__(self, o):
        return isinstance(o, K) and self.k == o.k

    def __hash__(self):
        return hash(self.k)

    def __repr__(self):
        return "K(%r)" % self.k


def build(n, make):
    out = None
    for i in range(n):
        out = dict(make())
    return out


class DictFromPairsTests(unittest.TestCase):
    def test_shapes(self):
        pairs = [("b", 1), ("a", 2), ("b", 3), (4, None)]
        for n in (1, 300):
            got = build(n, lambda: ((k, v) for k, v in pairs))
            self.assertEqual(list(got.items()), [("b", 3), ("a", 2), (4, None)])
            self.assertEqual(build(n, lambda: iter([["x", 1], "yz"])), {"x": 1, "y": "z"})
            self.assertEqual(build(n, lambda: zip("ab", [1, 2])), {"a": 1, "b": 2})
            self.assertEqual(build(n, lambda: (p for p in [])), {})
            ks = build(n, lambda: ((K(i % 2), i) for i in range(4)))
            self.assertEqual(ks, {K(0): 2, K(1): 3})

    def test_errors(self):
        with self.assertRaises(ValueError):
            build(300, lambda: (p for p in [("a", 1, 2)]))
        with self.assertRaises(TypeError):
            build(300, lambda: (p for p in [([], 1)]))
        with self.assertRaises(TypeError):
            build(300, lambda: (p for p in [1]))

        def gen():
            yield ("a", 1)
            raise RuntimeError("partway")

        with self.assertRaises(RuntimeError):
            build(300, gen)


if __name__ == "__main__":
    unittest.main()

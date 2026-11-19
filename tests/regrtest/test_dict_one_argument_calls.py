"""`dict(x)` with one positional argument, in compiled code.

A compiled `dict(x)` builds a mapping's copy, a lazy iterable's pairs
(`zip` over a generator, a generator of pairs) and an eager container's
pairs as before, with the same errors.
"""

import collections
import unittest

N = 3000


class Mapping:
    def keys(self):
        return ["a"]

    def __getitem__(self, k):
        return 1


def pairs():
    yield ("x", 1)
    yield ["y", 2]


def gen(n):
    for i in range(n):
        yield i


class DictOneArgumentTests(unittest.TestCase):
    def test_shapes(self):
        for _ in range(N):
            r = (
                dict([(1, 2)]),
                dict({"a": 1}),
                dict(Mapping()),
                dict(pairs()),
                dict(zip("abc", gen(3))),
                dict(collections.OrderedDict(z=1)),
                dict(iter([(3, 4)])),
            )
        self.assertEqual(r, ({1: 2}, {"a": 1}, {"a": 1}, {"x": 1, "y": 2},
                             {"a": 0, "b": 1, "c": 2}, {"z": 1}, {3: 4}))

    def test_errors(self):
        for _ in range(N // 10):
            with self.assertRaises(ValueError):
                dict([(1, 2, 3)])
            with self.assertRaises(TypeError):
                dict(5)
            with self.assertRaises(TypeError):
                dict([([], 1)])
            with self.assertRaises(ValueError):
                dict(x for x in ["abc"])


if __name__ == "__main__":
    unittest.main()

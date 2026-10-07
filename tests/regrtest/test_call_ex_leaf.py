"""`f(*args, **kwargs)` of a small function, evaluated in place.

A pure or effect leaf called through a spread tuple of its arity and no
keywords runs without an activation; other shapes (keywords, a wrong
arity, defaults) keep the full call, with its errors.
"""

import unittest


def add(a, b):
    return a + b


class Box:
    def __init__(self):
        self.v = 0


def store(box, v):
    box.v = v


def with_default(a, b=10):
    return a + b


def forward(*args, **kwargs):
    return add(*args, **kwargs)


class CallExLeafTest(unittest.TestCase):
    def test_spread(self):
        t = (1, 2)
        for _ in range(500):
            self.assertEqual(add(*t), 3)
            self.assertEqual(forward(3, 4), 7)
            self.assertEqual(add(*t, **{}), 3)

    def test_effects_once(self):
        b = Box()
        for i in range(300):
            store(*(b, i))
        self.assertEqual(b.v, 299)

    def test_other_shapes(self):
        for _ in range(200):
            self.assertEqual(add(*(1,), **{"b": 5}), 6)
            self.assertEqual(with_default(*(1,)), 11)
            self.assertEqual(forward(1, b=2), 3)
        with self.assertRaises(TypeError):
            add(*(1, 2, 3))
        with self.assertRaises(TypeError):
            add(*(1,))
        with self.assertRaises(TypeError):
            add(*(1, 2), **{"a": 3})


if __name__ == "__main__":
    unittest.main()

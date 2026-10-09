"""Calls of small functions with keyword-only parameters.

Such a function evaluates without a frame, its keyword-only parameters
bound from the call's keywords or their defaults. A replaced
`__kwdefaults__`, a missing required keyword-only argument, and a
`**kwargs` collector must behave as the ordinary call does.
"""

import unittest


def scaled(a, b=2, *, c=3):
    return a * b + c


def required(a, *, key):
    return a + key


def collector(a, *, c=1, **extra):
    return a + c + len(extra)


class KwonlyLeafCallTest(unittest.TestCase):
    def test_defaults_and_keywords(self):
        total = 0
        for i in range(3000):
            total += scaled(i) + scaled(i, c=4) + scaled(i, 3, c=0)
        self.assertEqual(total, sum(i * 2 + 3 + i * 2 + 4 + i * 3 for i in range(3000)))

    def test_required(self):
        for i in range(3000):
            self.assertEqual(required(i, key=1), i + 1)
        with self.assertRaises(TypeError):
            required(1)

    def test_replaced_kwdefaults(self):
        def f(a, *, c=3):
            return a + c

        for i in range(2000):
            self.assertEqual(f(i), i + 3)
        f.__kwdefaults__ = {"c": 10}
        self.assertEqual(f(1), 11)
        f.__kwdefaults__ = None
        with self.assertRaises(TypeError):
            f(1)

    def test_collector(self):
        for i in range(2000):
            self.assertEqual(collector(i), i + 1)
            self.assertEqual(collector(i, c=2, x=1, y=2), i + 4)


if __name__ == "__main__":
    unittest.main()

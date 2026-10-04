"""Compiled code calls a compiled scalar function directly, defaults included.

A call that leaves trailing or keyword-skipped parameters out binds the
callee's scalar defaults in compiled code. Each loop runs long enough for
the tier-2 compiler to take the callee and then the caller; then the
callee's defaults or code change, and every call must see them.
"""

import unittest


def with_defaults(a, b=10, c=20):
    return a + b + c


def scaled(x, k=1.5):
    return x * k


def gated(a, flag=True):
    if flag:
        return a
    return -a


def positional_loop(n):
    t = 0
    for i in range(n):
        t += with_defaults(i)
    return t


def keyword_loop(n):
    t = 0
    for i in range(n):
        t += with_defaults(i, c=5)
    return t


def middle_keyword_loop(n):
    t = 0
    for i in range(n):
        t += with_defaults(i, b=1)
    return t


def float_loop(n):
    t = 0.0
    for i in range(n):
        t += scaled(float(i))
    return t


def bool_loop(n):
    t = 0
    for i in range(n):
        t += gated(i)
    return t


def triangle(n):
    # The sum of 0, 1, ..., n - 1.
    return n * (n - 1) // 2


class LeafDefaultTests(unittest.TestCase):
    N = 3000

    def setUp(self):
        self.saved = (
            with_defaults.__defaults__,
            with_defaults.__code__,
            scaled.__defaults__,
            gated.__defaults__,
        )

    def tearDown(self):
        (
            with_defaults.__defaults__,
            with_defaults.__code__,
            scaled.__defaults__,
            gated.__defaults__,
        ) = self.saved

    def test_defaults_bind(self):
        n = self.N
        self.assertEqual(positional_loop(n), triangle(n) + 30 * n)
        self.assertEqual(keyword_loop(n), triangle(n) + 15 * n)
        self.assertEqual(middle_keyword_loop(n), triangle(n) + 21 * n)
        self.assertEqual(float_loop(n), 1.5 * triangle(n))
        self.assertEqual(bool_loop(n), triangle(n))

    def test_defaults_reassigned(self):
        n = self.N
        self.assertEqual(positional_loop(n), triangle(n) + 30 * n)
        self.assertEqual(keyword_loop(n), triangle(n) + 15 * n)
        with_defaults.__defaults__ = (100, 200)
        self.assertEqual(positional_loop(n), triangle(n) + 300 * n)
        self.assertEqual(keyword_loop(n), triangle(n) + 105 * n)
        self.assertEqual(middle_keyword_loop(n), triangle(n) + 201 * n)
        with_defaults.__defaults__ = (1.5, 2)
        self.assertEqual(positional_loop(4), 6 + 4 * 3.5)
        with_defaults.__defaults__ = (2,)
        with self.assertRaises(TypeError):
            positional_loop(1)
        self.assertEqual(middle_keyword_loop(3), 3 + 3 * 3)
        with self.assertRaises(TypeError):
            keyword_loop(1)
        with_defaults.__defaults__ = None
        with self.assertRaises(TypeError):
            keyword_loop(1)

    def test_lanes_change(self):
        n = self.N
        self.assertEqual(float_loop(n), 1.5 * triangle(n))
        scaled.__defaults__ = (2,)
        self.assertEqual(float_loop(4), 12.0)
        self.assertEqual(bool_loop(n), triangle(n))
        gated.__defaults__ = (False,)
        self.assertEqual(bool_loop(4), -6)
        gated.__defaults__ = (0,)
        self.assertEqual(bool_loop(4), -6)

    def test_code_reassigned(self):
        def other(a, b=1, c=2):
            return a * b * c

        n = self.N
        self.assertEqual(positional_loop(n), triangle(n) + 30 * n)
        with_defaults.__code__ = other.__code__
        self.assertEqual(positional_loop(4), 10 * 20 * triangle(4))


if __name__ == "__main__":
    unittest.main()

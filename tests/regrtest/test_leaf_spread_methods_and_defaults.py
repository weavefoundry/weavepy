"""`f(*args)` and `obj.m(*args)` of small functions, with defaults filled in.

The frameless `CALL_FUNCTION_EX` path binds a bound method's receiver
first and fills trailing parameters from the function's positional
defaults; a rebound `__defaults__`, a descriptor-dispatched method or a
wrong arity must behave as the full call does.
"""

import unittest


class Box:
    def __init__(self, v):
        self.v = v

    def get(self, a, b=10):
        return (self.v, a, b)

    def pair(self, a, b):
        return (self.v, a, b)

    def bump(self, k=1):
        self.v += k
        return self.v


def add(a, b=2, c=3):
    return a + b * 10 + c * 100


class LeafSpreadTests(unittest.TestCase):
    ROUNDS = 5000

    def test_method_spread(self):
        b = Box(1)
        one, two, none = (5,), (5, 6), ()
        for _ in range(self.ROUNDS):
            self.assertEqual(b.get(*one), (1, 5, 10))
            self.assertEqual(b.get(*two), (1, 5, 6))
            self.assertEqual(b.pair(*two), (1, 5, 6))
            self.assertEqual(Box(2).get(*one), (2, 5, 10))
        b.v = 0
        for _ in range(100):
            b.bump(*none)
        self.assertEqual(b.v, 100)
        b.bump(*(5,))
        self.assertEqual(b.v, 105)

    def test_function_defaults(self):
        for _ in range(self.ROUNDS):
            self.assertEqual(add(*(1,)), 321)
            self.assertEqual(add(*(1, 0)), 301)
            self.assertEqual(add(*(1, 0, 0)), 1)
            self.assertEqual(add(*(1,), **{}), 321)

    def test_arity_errors(self):
        b = Box(1)
        for _ in range(3):
            with self.assertRaises(TypeError):
                b.pair(*(1,))
            with self.assertRaises(TypeError):
                b.get(*(1, 2, 3))
            with self.assertRaises(TypeError):
                add(*())

    def test_defaults_rebound(self):
        def f(a, b=1):
            return (a, b)

        for _ in range(self.ROUNDS):
            self.assertEqual(f(*(0,)), (0, 1))
        f.__defaults__ = (9,)
        self.assertEqual(f(*(0,)), (0, 9))
        f.__defaults__ = None
        with self.assertRaises(TypeError):
            f(*(0,))

    def test_receiver_kept_alive(self):
        def make():
            return Box(7).get

        for _ in range(self.ROUNDS):
            self.assertEqual(make()(*(1,)), (7, 1, 10))

    def test_static_and_class_methods(self):
        class C:
            @staticmethod
            def s(a, b=2):
                return (a, b)

            @classmethod
            def c(cls, a, b=3):
                return (cls.__name__, a, b)

        for _ in range(self.ROUNDS):
            self.assertEqual(C.s(*(1,)), (1, 2))
            self.assertEqual(C().c(*(1,)), ("C", 1, 3))
            self.assertEqual(C.c(*(1, 4)), ("C", 1, 4))


if __name__ == "__main__":
    unittest.main()

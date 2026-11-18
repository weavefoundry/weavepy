"""Constructor calls with keyword arguments from compiled code.

Compiled code binds `C(a, b=x)` straight into `__init__`'s locals by a
permutation of the keywords it remembers per function and names tuple.
These loops check other keyword orders, keyword-only parameters with
defaults, reassigned defaults, and the errors for a missing or
unexpected keyword.
"""

import unittest

N = 3000


class P:
    def __init__(self, name, kind, *, default=None, annotation="ann"):
        self.r = (name, kind, default, annotation)


class Q:
    def __init__(self, a, b=2, c=3):
        self.r = (a, b, c)


class CtorKeywordTests(unittest.TestCase):
    def test_orders_and_defaults(self):
        for i in range(N):
            r = (P("a", 1, default=i).r, P("b", 2, annotation=i).r,
                 P("c", 3, annotation=1, default=2).r, P(kind=4, name="d").r,
                 Q(1, c=i).r, Q(c=5, a=6).r)
        self.assertEqual(r, (("a", 1, N - 1, "ann"), ("b", 2, None, N - 1),
                             ("c", 3, 2, 1), ("d", 4, None, "ann"),
                             (1, 2, N - 1), (6, 2, 5)))

    def test_reassigned_defaults(self):
        class R:
            def __init__(self, a, *, k=1, j=2):
                self.r = (a, k, j)

        for i in range(N):
            r = R(i, j=0).r
        self.assertEqual(r, (N - 1, 1, 0))
        R.__init__.__kwdefaults__ = {"k": 10, "j": 20}
        for i in range(N):
            r = R(i, j=0).r
        self.assertEqual(r, (N - 1, 10, 0))
        R.__init__.__kwdefaults__ = None
        with self.assertRaises(TypeError):
            R(1, j=0)

    def test_errors(self):
        for i in range(N):
            Q(1, b=i)
        with self.assertRaises(TypeError):
            Q(b=1)
        with self.assertRaises(TypeError):
            Q(1, d=1)
        with self.assertRaises(TypeError):
            P("x", 1, default=1, default2=2)


if __name__ == "__main__":
    unittest.main()

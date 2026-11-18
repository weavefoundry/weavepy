"""`functools.partial(func, *args)` builds in place.

The exact class's plain construction over a callable that isn't itself a
partial, without placeholders, lays the instance out as the native
`__new__` does; nested partials, placeholders, keywords, subclasses and
errors keep the full constructor.
"""

import functools
import pickle
import unittest
from functools import Placeholder, partial

N = 3000


def f(a, b, c=3):
    return (a, b, c)


class P2(partial):
    pass


class PartialConstructionTests(unittest.TestCase):
    def test_shapes(self):
        for _ in range(N):
            p = partial(f, 1)
            q = partial(p, 2)
            r = partial(f, Placeholder, 5)
            s = partial(f, c=9)
            t = P2(f, 1)
        self.assertEqual((p(2), q(), r(7), s(1, 2), t(2)), ((1, 2, 3),) * 2 + ((7, 5, 3), (1, 2, 9), (1, 2, 3)))
        self.assertIs(p.func, f)
        self.assertEqual((p.args, p.keywords, q.args), ((1,), {}, (1, 2)))
        self.assertIs(type(t), P2)
        self.assertEqual(p.__dict__, {})

    def test_errors_and_pickle(self):
        with self.assertRaises(TypeError):
            partial(1)
        with self.assertRaises(TypeError):
            partial()
        self.assertEqual(pickle.loads(pickle.dumps(partial(max, 1)))(5), 5)


if __name__ == "__main__":
    unittest.main()

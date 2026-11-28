"""Generators whose bodies call Python code at every step.

A generator body that subscripts an object with a Python `__getitem__`
(`collections.abc.Sequence.__iter__`, behind `x in path.parents`) can't
finish a native step without a call; once its steps keep stopping there,
a loop over it resumes it so the call is made directly. Results, the
`IndexError` that ends the iteration, a body whose receivers change kind,
and `Sequence`'s `in` and iteration all behave as before.
"""

import unittest
from collections.abc import Sequence

N = 3000


class Seq:
    def __init__(self, n):
        self.n = n

    def __getitem__(self, i):
        if i >= self.n:
            raise IndexError(i)
        return i * 2


def gen_try(s):
    i = 0
    try:
        while True:
            v = s[i]
            yield v
            i += 1
    except IndexError:
        return


def gen_bounded(s, n):
    for i in range(n):
        yield s[i]


class Parents(Sequence):
    def __init__(self, items):
        self._items = items

    def __len__(self):
        return len(self._items)

    def __getitem__(self, i):
        return self._items[i]


class GeneratorStepStallTests(unittest.TestCase):
    def test_python_getitem(self):
        s = Seq(5)
        for _ in range(N):
            a = [v for v in gen_try(s)]
            b = list(gen_bounded(s, 5))
            total = 0
            for v in gen_try(s):
                total += v
        self.assertEqual(a, [0, 2, 4, 6, 8])
        self.assertEqual(b, a)
        self.assertEqual(total, 20)

    def test_receivers_change_kind(self):
        out = []
        for k in range(N):
            src = Seq(3) if k % 3 else [10, 20, 30]
            out.append(sum(v for v in gen_bounded(src, 3)))
        self.assertEqual(out[:3], [60, 6, 6])
        self.assertEqual(sum(out), (N // 3) * 60 + (N - N // 3) * 6)

    def test_errors_propagate(self):
        s = Seq(2)
        for _ in range(500):
            with self.assertRaises(IndexError):
                list(gen_bounded(s, 3))
            g = gen_bounded(s, 3)
            self.assertEqual((next(g), next(g)), (0, 2))
            with self.assertRaises(IndexError):
                next(g)

    def test_sequence_mixins(self):
        p = Parents(["/a/b", "/a", "/"])
        for _ in range(N):
            r = ("/a" in p, "/x" in p, list(p), p.index("/"), p.count("/a"))
        self.assertEqual(r, (True, False, ["/a/b", "/a", "/"], 2, 1))

    def test_generator_close_mid_iteration(self):
        s = Seq(100)
        for _ in range(N):
            g = gen_try(s)
            for v in g:
                if v >= 4:
                    break
            g.close()
        self.assertEqual(v, 4)


if __name__ == "__main__":
    unittest.main()

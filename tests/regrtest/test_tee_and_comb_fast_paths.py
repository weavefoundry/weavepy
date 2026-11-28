"""itertools.tee built natively and math.comb / math.perm over machine
ints, from compiled loops: branches that share one buffer whatever order
they are read in, a tee of a tee (a copy), n of 0, 1 and 3, a generator
source, copy.copy and pickling of a branch, isinstance, bad arguments,
and comb / perm at the edges of 64-bit results and past them, each
matching CPython."""

import copy
import itertools
import math
import pickle
import unittest
import warnings


def tees(n, src, k=2):
    out = None
    for i in range(n):
        out = itertools.tee(src, k)
    return out


class TeeTests(unittest.TestCase):
    def test_branches_share_a_buffer(self):
        for n in (1, 300):
            a, b = tees(n, [1, 2, 3])
            self.assertEqual(next(a), 1)
            self.assertEqual(list(b), [1, 2, 3])
            self.assertEqual(list(a), [2, 3])
            self.assertEqual([list(t) for t in tees(n, [], 3)], [[], [], []])
            self.assertEqual(len(tees(n, "ab", 3)), 3)
            self.assertEqual(tees(n, "ab", 0), ())
            (only,) = tees(n, range(3), 1)
            self.assertEqual(list(only), [0, 1, 2])

    def test_generator_source_and_type(self):
        def gen():
            yield from "xyz"

        for n in (1, 300):
            a, b = tees(n, gen())
            self.assertIsInstance(a, type(itertools.tee([])[0]))
            self.assertEqual(list(zip(a, b)), [("x", "x"), ("y", "y"), ("z", "z")])

    def test_tee_of_a_tee_copies(self):
        a, b = itertools.tee(iter([1, 2, 3, 4]))
        next(a)
        c, d = itertools.tee(a)
        self.assertEqual(list(c), [2, 3, 4])
        self.assertEqual(list(d), [2, 3, 4])
        self.assertEqual(list(a), [2, 3, 4])
        self.assertEqual(list(b), [1, 2, 3, 4])

    def test_copy_and_pickle(self):
        a, b = tees(300, [5, 6, 7])
        next(a)
        c = copy.copy(a)
        self.assertEqual(list(c), [6, 7])
        self.assertEqual(list(a), [6, 7])
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", DeprecationWarning)
            try:
                d = pickle.loads(pickle.dumps(b))
            except TypeError:
                d = None
        if d is not None:
            self.assertEqual(list(d), [5, 6, 7])

    def test_weakrefs(self):
        import gc
        import weakref

        for n in (1, 300):
            a, b = tees(n, "abc")
            p = weakref.proxy(a)
            self.assertEqual(getattr(p, "__class__"), type(b))
            r = weakref.ref(b)
            self.assertIs(r(), b)
            del a
            gc.collect()
            with self.assertRaises(ReferenceError):
                getattr(p, "__class__")

    def test_bad_arguments(self):
        with self.assertRaises(ValueError):
            itertools.tee([1], -1)
        with self.assertRaises(TypeError):
            itertools.tee(3)
        with self.assertRaises(TypeError):
            itertools.tee([1], "2")


def ref_comb(a, b):
    if b > a:
        return 0
    r = 1
    for i in range(min(b, a - b)):
        r = r * (a - i) // (i + 1)
    return r


def ref_perm(a, b):
    if b > a:
        return 0
    r = 1
    for i in range(b):
        r *= a - i
    return r


def combs(n, a, b):
    out = None
    for i in range(n):
        out = (math.comb(a, b), math.perm(a, b))
    return out


class CombPermTests(unittest.TestCase):
    def test_values(self):
        cases = [(10, 3), (5, 0), (5, 5), (3, 7), (0, 0), (60, 30), (67, 33),
                 (100, 50), (2**62, 1), (2**63 - 1, 2), (40, 20), (25, 25)]
        for a, b in cases:
            want = (ref_comb(a, b), ref_perm(a, b))
            for n in (1, 300):
                self.assertEqual(combs(n, a, b), want, (a, b))
        self.assertEqual(math.perm(5), 120)
        self.assertEqual(math.comb(True, False), 1)

    def test_isqrt_and_gcd(self):
        def roots(n, x):
            out = None
            for i in range(n):
                out = math.isqrt(x)
            return out

        def ref_isqrt(x):
            lo, hi = 0, x + 1
            while hi - lo > 1:
                mid = (lo + hi) // 2
                if mid * mid <= x:
                    lo = mid
                else:
                    hi = mid
            return lo

        values = [0, 1, 2, 3, 4, 15, 16, 17, 10**12, 10**12 - 1, 2**52 + 1, 2**53 - 1,
                  2**62, 2**63 - 1, (2**31 - 1) ** 2, (2**31 - 1) ** 2 - 1, 4503599761588224]
        for x in values:
            for n in (1, 300):
                self.assertEqual(roots(n, x), ref_isqrt(x), x)
        with self.assertRaises(ValueError):
            roots(300, -1)

        def gcds(n, *args):
            out = None
            for i in range(n):
                out = math.gcd(*args)
            return out

        for n in (1, 300):
            self.assertEqual(gcds(n, 84, 36), 12)
            self.assertEqual(gcds(n, 84, 36, 10), 2)
            self.assertEqual(gcds(n), 0)
            self.assertEqual(gcds(n, 0, 0), 0)
            self.assertEqual(gcds(n, -12, 18), 6)
            self.assertEqual(gcds(n, -(2**63), 0), 2**63)
            self.assertEqual(gcds(n, 2**70, 2**65), 2**65)
            self.assertEqual(gcds(n, True, 4), 1)
        with self.assertRaises(TypeError):
            gcds(300, 1.5, 2)

    def test_errors(self):
        with self.assertRaises(ValueError):
            combs(300, -1, 2)
        with self.assertRaises(ValueError):
            combs(300, 3, -2)
        with self.assertRaises(TypeError):
            math.comb(3.0, 1)


if __name__ == "__main__":
    unittest.main()

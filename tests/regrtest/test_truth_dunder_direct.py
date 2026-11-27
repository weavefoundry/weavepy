"""Compiled truth tests call a class's Python `__bool__` (or `__len__`)
directly; the truth, the protocol's errors for a bad result, raises
(caught in the testing frame or past it), tests of locals and of
computed values, `__bool__ = None`, and methods replaced later match the
general test."""

import unittest


class B:
    def __init__(self, v):
        self.v = v
        self.n = 0

    def __bool__(self):
        self.n += 1
        return self.v


class L:
    def __init__(self, n):
        self.items = list(range(n))

    def __len__(self):
        return len(self.items)


class BadBool:
    def __bool__(self):
        self.called = True
        return 1


class BadLen:
    def __init__(self, r):
        self.r = r

    def __len__(self):
        self.called = True
        return self.r


class Raiser:
    def __bool__(self):
        raise KeyError("no truth")


class Blocked:
    __bool__ = None


def make(v):
    return B(v)


class TruthDunderDirect(unittest.TestCase):
    def test_bool_local_and_global(self):
        t, f = B(True), B(False)
        n = 0
        for i in range(3000):
            if t:
                n += 1
            if f:
                n += 100
            if not f:
                n += 1
        self.assertEqual(n, 6000)
        self.assertEqual((t.n, f.n), (3000, 6000))

    def test_bool_of_computed_value(self):
        n = 0
        for i in range(3000):
            if make(i % 3 == 0):
                n += 1
        self.assertEqual(n, 1000)

    def test_len_truth(self):
        empty, full = L(0), L(3)
        n = 0
        for i in range(3000):
            if empty:
                n += 100
            if full:
                n += 1
        self.assertEqual(n, 3000)

    def test_bad_results(self):
        for i in range(500):
            with self.assertRaises(TypeError) as cm:
                if BadBool():
                    pass
            self.assertEqual(str(cm.exception), "__bool__ should return bool, returned int")
            with self.assertRaises(ValueError) as cm:
                if BadLen(-1):
                    pass
            self.assertEqual(str(cm.exception), "__len__() should return >= 0")
            with self.assertRaises(TypeError) as cm:
                if BadLen("x"):
                    pass
            self.assertEqual(str(cm.exception), "'str' object cannot be interpreted as an integer")
            with self.assertRaises(OverflowError):
                if BadLen(2**70):
                    pass

    def test_raise_caught_here(self):
        r = Raiser()
        n = 0
        for i in range(2000):
            try:
                if r:
                    n += 100
            except KeyError:
                n += 1
        self.assertEqual(n, 2000)

    def test_raise_past_frame(self):
        def check(o):
            if o:
                return "yes"
            return "no"

        r, t = Raiser(), B(True)
        for i in range(2000):
            with self.assertRaises(KeyError):
                check(r)
            self.assertEqual(check(t), "yes")

    def test_blocked(self):
        b = Blocked()
        for i in range(300):
            with self.assertRaises(TypeError):
                if b:
                    pass

    def test_while_and_and_or(self):
        for _ in range(300):
            b = B(True)
            k = 0
            while b and k < 5:
                k += 1
            self.assertEqual(k, 5)
            self.assertIs(B(False) or 7, 7)
            x = B(True)
            self.assertIs(x and 8, 8)

    def test_method_replaced(self):
        class C:
            def __bool__(self):
                return True

        c = C()
        n = 0
        for i in range(1000):
            if c:
                n += 1
        C.__bool__ = lambda self: False
        for i in range(1000):
            if c:
                n += 100
        del C.__bool__
        C.__len__ = lambda self: 0
        for i in range(1000):
            if c:
                n += 10000
        del C.__len__
        for i in range(1000):
            if c:
                n += 1
        self.assertEqual(n, 2000)

    def test_generator_caller(self):
        t = B(True)

        def g():
            for i in range(500):
                if t:
                    yield i

        self.assertEqual(sum(g()), sum(range(500)))


if __name__ == "__main__":
    unittest.main()

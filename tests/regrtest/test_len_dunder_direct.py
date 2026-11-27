"""Compiled `len(obj)` calls a class's Python `__len__` directly; the
lengths, the errors for a bad result, raises (caught in the frame or
past it), `len` shadowed or replaced, and a method replaced later match
the general call."""

import builtins
import unittest


class Sized:
    def __init__(self, n):
        self.n = n
        self.calls = 0

    def __len__(self):
        self.calls += 1
        return self.n


class Bad:
    def __init__(self, r):
        self.r = r

    def __len__(self):
        self.seen = True
        return self.r


class Raiser:
    def __len__(self):
        raise KeyError("no length")


class IntSub(int):
    pass


class LenDunderDirect(unittest.TestCase):
    def test_lengths(self):
        s = Sized(7)
        total = 0
        for i in range(3000):
            total += len(s)
        self.assertEqual(total, 21000)
        self.assertEqual(s.calls, 3000)

    def test_result_kinds(self):
        for i in range(500):
            self.assertEqual(len(Bad(True)), 1)
            self.assertEqual(len(Bad(IntSub(5))), 5)
            self.assertIs(type(len(Bad(IntSub(5)))), int)
            with self.assertRaises(ValueError) as cm:
                len(Bad(-1))
            self.assertEqual(str(cm.exception), "__len__() should return >= 0")
            with self.assertRaises(TypeError) as cm:
                len(Bad(1.5))
            self.assertEqual(str(cm.exception), "'float' object cannot be interpreted as an integer")
            with self.assertRaises(OverflowError):
                len(Bad(2**70))

    def test_raises(self):
        r = Raiser()
        n = 0
        for i in range(1000):
            try:
                n += len(r)
            except KeyError:
                n += 1
        self.assertEqual(n, 1000)

        def size(o):
            return len(o) + 1

        for i in range(1000):
            with self.assertRaises(KeyError):
                size(r)
            self.assertEqual(size(Sized(2)), 3)

    def test_mixed_receivers(self):
        s = Sized(3)
        items = [s, [1, 2], "abcd", {1: 2}, s]
        for _ in range(600):
            self.assertEqual([len(x) for x in items], [3, 2, 4, 1, 3])

    def test_len_shadowed(self):
        s = Sized(4)
        real = builtins.len
        try:
            for i in range(500):
                self.assertEqual(len(s), 4)
            builtins.len = lambda o: -1
            self.assertEqual(len(s), -1)
        finally:
            builtins.len = real
        self.assertEqual(len(s), 4)

    def test_method_replaced(self):
        class C:
            def __len__(self):
                return 1

        c = C()
        n = 0
        for i in range(1000):
            n += len(c)
        C.__len__ = lambda self: 10
        for i in range(1000):
            n += len(c)
        del C.__len__
        with self.assertRaises(TypeError):
            len(c)
        self.assertEqual(n, 11000)

    def test_generator_caller(self):
        s = Sized(2)

        def g():
            for i in range(500):
                yield len(s)

        self.assertEqual(sum(g()), 1000)


if __name__ == "__main__":
    unittest.main()

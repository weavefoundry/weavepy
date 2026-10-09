"""`len(obj)` and `obj[key]` through a class's Python `__len__` and
`__getitem__`, on the fast paths and off them.

A plain Python method runs with the instance (a pure one in place); its
result still goes through the checks `len()` applies, errors it raises
propagate, and a method replaced on the class (or a callable that isn't
a plain function) takes effect at once.
"""

import unittest


class Seq:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        return self.n

    def __getitem__(self, i):
        if i >= self.n:
            raise IndexError(i)
        return i * 2


class Counted:
    calls = 0

    def __len__(self):
        Counted.calls += 1
        return 3

    def __getitem__(self, k):
        Counted.calls += 1
        return k


class BadLen:
    def __init__(self, v):
        self.v = v

    def __len__(self):
        return self.v


class ContainerDunderTest(unittest.TestCase):
    def test_len_and_getitem(self):
        s = Seq(5)
        for _ in range(500):
            self.assertEqual(len(s), 5)
            self.assertEqual(s[3], 6)
        with self.assertRaises(IndexError):
            s[9]
        self.assertEqual(list(s), [0, 2, 4, 6, 8])

    def test_effects_happen_once(self):
        c = Counted()
        Counted.calls = 0
        for _ in range(400):
            len(c)
            c[1]
        self.assertEqual(Counted.calls, 800)

    def test_len_result_checks(self):
        for _ in range(200):
            self.assertEqual(len(BadLen(True)), 1)
        with self.assertRaises(ValueError):
            len(BadLen(-1))
        with self.assertRaises(TypeError):
            len(BadLen("x"))
        with self.assertRaises(OverflowError):
            len(BadLen(2**70))

    def test_replaced_methods(self):
        class C:
            def __len__(self):
                return 1

            def __getitem__(self, k):
                return "a"

        c = C()
        for _ in range(300):
            self.assertEqual(len(c), 1)
            self.assertEqual(c[0], "a")
        C.__len__ = lambda self: 7
        C.__getitem__ = lambda self, k: "b"
        self.assertEqual(len(c), 7)
        self.assertEqual(c[0], "b")
        C.__getitem__ = staticmethod(lambda k: "s")
        self.assertEqual(c[0], "s")
        del C.__len__
        with self.assertRaises(TypeError):
            len(c)


if __name__ == "__main__":
    unittest.main()

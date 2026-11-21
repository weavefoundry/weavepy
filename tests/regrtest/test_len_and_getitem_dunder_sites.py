"""`len(obj)` and `obj[i]` through a Python `__len__` / `__getitem__`.

Compiled `len()` sites evaluate a frameless `__len__` in place, and
compiled subscripts enter a warm `__getitem__`'s compiled plan; results,
errors and the checks on `__len__`'s result behave as before.
"""

import unittest

N = 30000


class Seq:
    def __init__(self, d):
        self.d = d

    def __len__(self):
        return len(self.d)

    def __getitem__(self, i):
        return self.d[i]


class Neg:
    def __len__(self):
        return -1


class Bad:
    def __len__(self):
        raise KeyError("len")


class Big:
    def __len__(self):
        return 2 ** 70


class DunderSiteTests(unittest.TestCase):
    def test_values(self):
        s = Seq([1, 2, 3])
        total = 0
        for i in range(N):
            total += len(s) + s[i % 3]
        self.assertEqual(total, N * 3 + (N // 3) * 6)
        s.d = [9]
        self.assertEqual((len(s), s[0], s[-1]), (1, 9, 9))

    def test_errors(self):
        s = Seq([1])
        for _ in range(N // 100):
            with self.assertRaises(IndexError):
                s[5]
            with self.assertRaises(ValueError):
                len(Neg())
            with self.assertRaises(KeyError):
                len(Bad())
            with self.assertRaises(OverflowError):
                len(Big())


if __name__ == "__main__":
    unittest.main()

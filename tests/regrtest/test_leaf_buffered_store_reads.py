"""Reads after a buffered store in a method evaluated without a frame.

A small method's attribute stores wait for its return, so a later read of
the same attribute must see the stored value, while a read of any other
attribute (or of the same name on another object) sees the object's own
field. The compiled form serves those other reads from its field cache.
"""

import unittest


class Vec:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def shift(self, other):
        self.x = self.x + 1.0
        a = self.y
        b = self.x
        c = other.x
        self.y = a + b + c
        return b

    def norm(self):
        x = self.x
        y = self.y
        n = (x * x + y * y) ** 0.5
        self.x /= n
        self.y /= n


class BufferedStoreReadTest(unittest.TestCase):
    def test_reads_see_pending_and_current_values(self):
        v, w = Vec(0.0, 0.0), Vec(10.0, 0.0)
        for i in range(3000):
            b = v.shift(w)
            self.assertEqual(b, float(i + 1))
        self.assertEqual(v.x, 3000.0)
        self.assertEqual(v.y, sum(range(1, 3001)) + 3000 * 10.0)
        self.assertEqual(w.x, 10.0)

    def test_self_as_other(self):
        v = Vec(0.0, 0.0)
        for _ in range(2000):
            v.shift(v)
        # `other.x` is `self.x`: the pending value.
        self.assertEqual(v.x, 2000.0)
        self.assertEqual(v.y, 2.0 * sum(range(1, 2001)))

    def test_normalize(self):
        pts = [Vec(3.0 * (i + 1), 4.0 * (i + 1)) for i in range(2000)]
        for p in pts:
            p.norm()
        for p in pts:
            self.assertAlmostEqual(p.x, 0.6)
            self.assertAlmostEqual(p.y, 0.8)


if __name__ == "__main__":
    unittest.main()

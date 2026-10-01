"""Compiled code reads and writes instance fields in line.

Each loop runs long enough for the tier-2 compiler to take it, then the
receiver or its class changes shape under the compiled code: every change
must reach the generic path with the interpreter's result.
"""

import unittest


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y


class Other:
    def __init__(self):
        self.y = 100
        self.x = 200


def read_sum(p, n):
    t = 0
    for _ in range(n):
        t += p.x
    return t


def bump(p, n):
    for _ in range(n):
        p.y = p.y + 1.5
    return p.y


class InlineFieldTests(unittest.TestCase):
    N = 3000

    def test_plain_reads_and_writes(self):
        p = Point(2, 0.5)
        self.assertEqual(read_sum(p, self.N), 2 * self.N)
        self.assertEqual(bump(p, self.N), 0.5 + 1.5 * self.N)

    def test_class_gains_a_property(self):
        class C:
            def __init__(self):
                self.x = 1

        def loop(o, n):
            t = 0
            for _ in range(n):
                t += o.x
            return t

        c = C()
        self.assertEqual(loop(c, self.N), self.N)
        C.x = property(lambda self: 7)
        self.assertEqual(loop(c, self.N), 7 * self.N)

    def test_class_swap(self):
        class A:
            def __init__(self):
                self.x = 1
                self.y = 2

        class B:
            def __init__(self):
                self.y = 3
                self.x = 4

        def loop(o, n):
            t = 0
            for _ in range(n):
                t += o.x
            return t

        a = A()
        self.assertEqual(loop(a, self.N), self.N)
        b = B()
        self.assertEqual(loop(b, self.N), 4 * self.N)
        a.__class__ = B
        self.assertEqual(loop(a, self.N), self.N)

    def test_materialized_dict(self):
        p = Point(3, 1.0)
        self.assertEqual(read_sum(p, self.N), 3 * self.N)
        d = vars(p)
        d["x"] = 5
        self.assertEqual(read_sum(p, self.N), 5 * self.N)
        del d["y"]
        d["y"] = 1.0
        self.assertEqual(bump(p, 10), 16.0)

    def test_deleted_and_readded_field(self):
        p = Point(1, 0.0)
        self.assertEqual(read_sum(p, self.N), self.N)
        del p.x
        with self.assertRaises(AttributeError):
            read_sum(p, 1)
        p.x = 9
        self.assertEqual(read_sum(p, self.N), 9 * self.N)

    def test_non_scalar_store_and_read(self):
        p = Point(1, 0.0)
        self.assertEqual(bump(p, self.N), 1.5 * self.N)
        p.y = [1]
        with self.assertRaises(TypeError):
            bump(p, 1)
        p.y = 2.0
        self.assertEqual(bump(p, 2), 5.0)

    def test_field_order_differs_between_instances(self):
        p = Point(6, 0.0)
        o = Other()
        self.assertEqual(read_sum(p, self.N), 6 * self.N)
        self.assertEqual(read_sum(o, self.N), 200 * self.N)
        self.assertEqual(read_sum(p, self.N), 6 * self.N)

    def test_type_change_of_field(self):
        p = Point(1, 0.0)
        self.assertEqual(read_sum(p, self.N), self.N)
        p.x = 2.5
        self.assertEqual(read_sum(p, 4), 10.0)
        p.x = True
        self.assertEqual(read_sum(p, 4), 4)


if __name__ == "__main__":
    unittest.main()

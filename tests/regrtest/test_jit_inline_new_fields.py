"""Compiled constructors append a fresh instance's fields in line.

An `__init__` that sets scalar fields in its class's usual order stores
each one past the previous without calling a helper. Each loop runs long
enough for the tier-2 compiler to take the constructor; then instances
arrive in other shapes, and each must end up with exactly its fields.
"""

import unittest


class Point:
    def __init__(self, i):
        self.x = i
        self.y = i * 0.5
        self.z = i > 3


def build(cls, n):
    out = []
    for i in range(n):
        out.append(cls(i))
    return out


class InlineNewFieldTests(unittest.TestCase):
    N = 3000

    def check(self, p, i):
        self.assertEqual(vars(p), {"x": i, "y": i * 0.5, "z": i > 3})

    def test_fields(self):
        points = build(Point, self.N)
        for i in (0, 1, 4, self.N - 1):
            self.check(points[i], i)
        self.assertEqual(sum(p.x for p in points), self.N * (self.N - 1) // 2)

    def test_reinitialized_instance(self):
        points = build(Point, self.N)
        p = points[10]
        Point.__init__(p, 99)
        self.check(p, 99)

    def test_other_order_first(self):
        class Q:
            def __init__(self, i, flip=False):
                if flip:
                    self.b = -i
                    self.a = i
                else:
                    self.a = i
                    self.b = -i

        def make(n, flip):
            return [Q(i, flip) for i in range(n)]

        qs = make(self.N, False)
        self.assertEqual(vars(qs[5]), {"a": 5, "b": -5})
        flipped = make(10, True)
        self.assertEqual(list(vars(flipped[5]).items()), [("b", -5), ("a", 5)])
        again = make(10, False)
        self.assertEqual(list(vars(again[5]).items()), [("a", 5), ("b", -5)])

    def test_subclass_and_extra_fields(self):
        class Base:
            def __init__(self, i):
                self.u = i
                self.v = 2 * i

        class Sub(Base):
            pass

        bases = build(Base, self.N)
        subs = build(Sub, 10)
        self.assertEqual(vars(bases[7]), {"u": 7, "v": 14})
        self.assertEqual(vars(subs[7]), {"u": 7, "v": 14})
        b = Base(1)
        b.w = 3
        self.assertEqual(vars(Base(2)), {"u": 2, "v": 4})
        self.assertEqual(vars(b), {"u": 1, "v": 2, "w": 3})

    def test_class_attribute_becomes_a_property(self):
        class R:
            def __init__(self, i):
                self.k = i

        rs = build(R, self.N)
        self.assertEqual(rs[3].k, 3)
        seen = []
        R.k = property(lambda self: 0, lambda self, v: seen.append(v))
        r = R(5)
        self.assertEqual(seen, [5])
        self.assertEqual(r.k, 0)

    def test_instances_that_set_fewer_fields(self):
        class S:
            def __init__(self, i, full=True):
                if full:
                    self.a = i
                    self.b = 2 * i

        def make(n, full):
            return [S(i, full) for i in range(n)]

        full = make(self.N, True)
        self.assertEqual(vars(full[3]), {"a": 3, "b": 6})
        empty = make(10, False)
        self.assertEqual(vars(empty[3]), {})
        empty[3].b = 1
        empty[3].a = 2
        self.assertEqual(list(vars(empty[3]).items()), [("b", 1), ("a", 2)])
        self.assertEqual(vars(make(1, True)[0]), {"a": 0, "b": 0})

    def test_constructor_raising_midway(self):
        class E:
            def __init__(self, i):
                self.a = i
                if i < 0:
                    raise ValueError(i)
                self.b = i

        def make(n):
            return [E(i) for i in range(n)]

        self.assertEqual(vars(make(self.N)[5]), {"a": 5, "b": 5})
        with self.assertRaises(ValueError):
            E(-1)
        self.assertEqual(vars(make(3)[2]), {"a": 2, "b": 2})


if __name__ == "__main__":
    unittest.main()

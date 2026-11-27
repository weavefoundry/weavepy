"""Compiled `set.add` of an object whose Python `__hash__` evaluates
frameless hashes it in place; the members, equal-but-distinct objects
(their `__eq__` decides), hash collisions, `__hash__` returning big or
negative values, and lookups through the general paths afterwards match
the general call."""

import unittest


class K:
    def __init__(self, a, b):
        self.a = a
        self.b = b
        self.eqs = 0

    def key(self):
        return (self.a, self.b)

    def __hash__(self):
        return hash(self.key())

    def __eq__(self, o):
        self.eqs += 1
        return isinstance(o, K) and self.key() == o.key()


class Collide:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return 7

    def __eq__(self, o):
        return isinstance(o, Collide) and self.v == o.v


class Big:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return self.v

    def __eq__(self, o):
        return isinstance(o, Big) and self.v == o.v


class SetAddLeafHash(unittest.TestCase):
    def test_same_objects(self):
        ks = [K(i, -i) for i in range(50)]
        s = set()
        for _ in range(100):
            for k in ks:
                s.add(k)
        self.assertEqual(len(s), 50)
        for k in ks:
            self.assertIn(k, s)
        self.assertIn(K(3, -3), s)

    def test_equal_distinct_objects(self):
        s = set()
        for i in range(3000):
            s.add(K(i % 10, 0))
        self.assertEqual(len(s), 10)
        self.assertEqual(sorted(k.a for k in s), list(range(10)))

    def test_collisions(self):
        s = set()
        for i in range(3000):
            s.add(Collide(i % 5))
        self.assertEqual(len(s), 5)
        self.assertEqual(sorted(c.v for c in s), [0, 1, 2, 3, 4])

    def test_hash_values(self):
        s = set()
        vals = [0, -1, -2, 2**61 - 1, 2**61, -(2**70), 2**100 + 3]
        for _ in range(500):
            for v in vals:
                s.add(Big(v))
        self.assertEqual(len(s), len(vals))
        for v in vals:
            self.assertIn(Big(v), s)
        self.assertEqual(hash(Big(-1)), -2)
        d = {Big(v): v for v in vals}
        for b in s:
            self.assertEqual(d[b], b.v)

    def test_mixed_with_natives(self):
        s = set()
        for i in range(1000):
            s.add(i % 3)
            s.add(K(i % 3, 1))
            s.add("x")
        self.assertEqual(len(s), 7)
        s.discard(K(0, 1))
        self.assertEqual(len(s), 6)
        self.assertNotIn(K(0, 1), s)

    def test_frozen_and_subclass_sets(self):
        class S(set):
            pass

        t = S()
        for i in range(1000):
            t.add(K(i % 4, 2))
        self.assertEqual(len(t), 4)
        self.assertEqual(frozenset(t), frozenset(K(i, 2) for i in range(4)))


if __name__ == "__main__":
    unittest.main()

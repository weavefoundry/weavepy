"""Compiled attribute stores that replace an object.

A compiled `obj.attr = value` that replaces another object releases the
old one in place when another owner keeps it, and through the general
path when it was the last reference (its finalizer runs at once).
Reference counts, finalizers and the stored values stay right.
"""

import sys
import unittest

N = 3000


class Node:
    pass


class Dying:
    deaths = []

    def __init__(self, n):
        self.n = n

    def __del__(self):
        Dying.deaths.append(self.n)


class FieldReplaceTests(unittest.TestCase):
    def test_shared_old_values(self):
        nodes = [Node() for _ in range(50)]
        for x in nodes:
            x.ref = x
        keep = [Node() for _ in range(50)]
        base = [sys.getrefcount(k) for k in keep]
        for i in range(N // 10):
            for x, k in zip(nodes, keep):
                x.ref = k
            for x in nodes:
                x.ref = x
        del x, k
        self.assertEqual([sys.getrefcount(k) for k in keep], base)
        self.assertTrue(all(x.ref is x for x in nodes))
        for x, k in zip(nodes, keep):
            x.ref = k
        del x, k
        self.assertEqual([sys.getrefcount(k) for k in keep], [b + 1 for b in base])

    def test_last_reference_finalizes(self):
        Dying.deaths.clear()
        holder = Node()
        holder.d = Dying(-1)
        for i in range(N):
            holder.d = Dying(i)
            self.assertEqual(len(Dying.deaths), i + 1)
        self.assertEqual(Dying.deaths[:3], [-1, 0, 1])
        self.assertEqual(holder.d.n, N - 1)

    def test_mixed_values(self):
        n = Node()
        n.v = 0
        objs = ["s", 1.5, (1, 2), [3], None, 7, n]
        for i in range(N):
            n.v = objs[i % len(objs)]
        self.assertIs(n.v, objs[(N - 1) % len(objs)])
        self.assertEqual(sys.getrefcount(objs[3]), sys.getrefcount([3]) + 2)


if __name__ == "__main__":
    unittest.main()

"""Compiled code compares objects with ints, and objects by identity.

An integer compared with a value the compiler only knows as an object (a
class constant, an attribute read generically) is guarded to be an exact
machine `int`; `a is b` on two objects compares identities. Each loop
here runs long enough to compile; then the values change shape, and the
results must still match what the interpreter computes.
"""

import unittest

WARM = 3000


class Dir:
    NONE = 0
    FORWARD = 1


class Edge:
    def __init__(self, a, b, d):
        self.a = a
        self.b = b
        self.d = d


def pick(e, n):
    t = 0
    for _ in range(n):
        if e.d == Dir.FORWARD:
            t += 1
        else:
            t -= 1
    return t


def ahead(e, n):
    t = 0
    for i in range(n):
        if Dir.FORWARD < i:
            t += 1
    return t


def count_same(items, target):
    t = 0
    for x in items:
        if x is target:
            t += 1
        if x is not target:
            t -= 1
    return t


class ObjectCompareTests(unittest.TestCase):
    def test_class_constant_compare(self):
        e = Edge(1, 2, 1)
        self.assertEqual(pick(e, WARM), WARM)
        e.d = 0
        self.assertEqual(pick(e, 10), -10)
        saved = Dir.FORWARD
        try:
            Dir.FORWARD = "fwd"
            e.d = "fwd"
            self.assertEqual(pick(e, 10), 10)
            Dir.FORWARD = 2**70
            e.d = 2**70
            self.assertEqual(pick(e, 10), 10)
            Dir.FORWARD = 1.0
            e.d = 1
            self.assertEqual(pick(e, 10), 10)
            Dir.FORWARD = True
            self.assertEqual(pick(e, 10), 10)
        finally:
            Dir.FORWARD = saved
        self.assertEqual(ahead(e, WARM), WARM - 2)
        Dir.FORWARD = 5
        try:
            self.assertEqual(ahead(e, 10), 4)
            Dir.FORWARD = None
            with self.assertRaises(TypeError):
                ahead(e, 10)
        finally:
            Dir.FORWARD = saved

    def test_identity(self):
        a, b = Edge(1, 2, 0), Edge(1, 2, 0)
        items = [a, b, a, None, a]
        self.assertEqual(count_same(items * 600, a), 600)
        self.assertEqual(count_same([b, b, None], b), 1)
        self.assertEqual(count_same([None, None, a], None), 1)
        self.assertEqual(count_same([[], [], a], a), -1)
        lst = []
        self.assertEqual(count_same([lst, [], lst], lst), 1)
        self.assertEqual(count_same(["x", "y", "x"], "x"), 1)


if __name__ == "__main__":
    unittest.main()
